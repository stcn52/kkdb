// R17 – High Availability & Disaster Recovery:
//   - Automatic failover chain
//   - Read-only replica synchronization
//   - Cross-region disaster recovery
//   - Rolling upgrade coordination
//   - Health probe system
//
// Provides:
//   - `FailoverChain`: ordered failover candidates with priority/health
//   - `ReplicaSyncer`: tracks read-only replica sync positions
//   - `CrossRegionDR`: cross-region replication with RPO/RTO tracking
//   - `RollingUpgradeCoordinator`: phased rolling upgrades
//   - `HealthProbe`: extensible health check system

use std::collections::HashMap;

// ── Automatic Failover Chain ──────────────────────────────────────────

/// Failover candidate node.
#[derive(Debug, Clone)]
pub struct FailoverCandidate {
    pub node_id: String,
    pub priority: u32,
    pub is_healthy: bool,
    pub last_sync_lsn: u64,
    pub region: String,
}

/// Manages ordered failover candidates.
pub struct FailoverChain {
    candidates: Vec<FailoverCandidate>,
    current_leader: Option<String>,
}

impl FailoverChain {
    pub fn new() -> Self {
        Self { candidates: Vec::new(), current_leader: None }
    }

    pub fn set_leader(&mut self, node_id: &str) {
        self.current_leader = Some(node_id.to_string());
    }

    pub fn add_candidate(&mut self, candidate: FailoverCandidate) {
        self.candidates.push(candidate);
        self.candidates.sort_by(|a, b| a.priority.cmp(&b.priority));
    }

    pub fn update_health(&mut self, node_id: &str, healthy: bool) {
        if let Some(c) = self.candidates.iter_mut().find(|c| c.node_id == node_id) {
            c.is_healthy = healthy;
        }
    }

    pub fn update_sync_lsn(&mut self, node_id: &str, lsn: u64) {
        if let Some(c) = self.candidates.iter_mut().find(|c| c.node_id == node_id) {
            c.last_sync_lsn = lsn;
        }
    }

    /// Select the best failover candidate (highest priority healthy node with most recent LSN).
    pub fn select_failover(&self) -> Option<&FailoverCandidate> {
        self.candidates.iter()
            .filter(|c| c.is_healthy)
            .filter(|c| self.current_leader.as_deref() != Some(&c.node_id))
            .max_by_key(|c| c.last_sync_lsn)
    }

    /// Perform failover: returns new leader node_id.
    pub fn failover(&mut self) -> Option<String> {
        let new_leader = self.select_failover()?.node_id.clone();
        self.current_leader = Some(new_leader.clone());
        Some(new_leader)
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub fn current_leader(&self) -> Option<&str> {
        self.current_leader.as_deref()
    }
}

// ── Read-Only Replica Sync ────────────────────────────────────────────

/// Sync state of a replica.
#[derive(Debug, Clone)]
pub struct ReplicaSync {
    pub replica_id: String,
    pub applied_lsn: u64,
    pub replay_lag_ms: u64,
    pub is_streaming: bool,
}

/// Tracks read-only replica synchronization.
pub struct ReplicaSyncer {
    replicas: HashMap<String, ReplicaSync>,
    primary_lsn: u64,
}

impl ReplicaSyncer {
    pub fn new() -> Self {
        Self { replicas: HashMap::new(), primary_lsn: 0 }
    }

    pub fn set_primary_lsn(&mut self, lsn: u64) {
        self.primary_lsn = lsn;
    }

    pub fn add_replica(&mut self, replica_id: &str) {
        self.replicas.insert(replica_id.to_string(), ReplicaSync {
            replica_id: replica_id.to_string(),
            applied_lsn: 0,
            replay_lag_ms: 0,
            is_streaming: false,
        });
    }

    pub fn update_replica(&mut self, replica_id: &str, applied_lsn: u64, lag_ms: u64) {
        if let Some(r) = self.replicas.get_mut(replica_id) {
            r.applied_lsn = applied_lsn;
            r.replay_lag_ms = lag_ms;
            r.is_streaming = true;
        }
    }

    /// Compute replication lag in LSN units.
    pub fn lsn_lag(&self, replica_id: &str) -> Option<u64> {
        self.replicas.get(replica_id)
            .map(|r| self.primary_lsn.saturating_sub(r.applied_lsn))
    }

    /// Find replicas lagging beyond threshold.
    pub fn lagging_replicas(&self, max_lag_ms: u64) -> Vec<&str> {
        self.replicas.values()
            .filter(|r| r.replay_lag_ms > max_lag_ms)
            .map(|r| r.replica_id.as_str())
            .collect()
    }

    pub fn replica_count(&self) -> usize {
        self.replicas.len()
    }

    /// Is quorum of replicas in sync (within lsn_delta)?
    pub fn quorum_in_sync(&self, lsn_delta: u64) -> bool {
        let in_sync = self.replicas.values()
            .filter(|r| self.primary_lsn.saturating_sub(r.applied_lsn) <= lsn_delta)
            .count();
        let total = self.replicas.len();
        if total == 0 { return false; }
        in_sync > total / 2
    }
}

// ── Cross-Region DR ───────────────────────────────────────────────────

/// Cross-region replication config for a region.
#[derive(Debug, Clone)]
pub struct RegionConfig {
    pub region_name: String,
    pub endpoint: String,
    pub rpo_target_s: u64,
    pub rto_target_s: u64,
    pub last_replicated_lsn: u64,
    pub last_replicated_ts: u64,
}

/// Cross-region disaster recovery manager.
pub struct CrossRegionDR {
    primary_region: String,
    regions: HashMap<String, RegionConfig>,
    current_ts: u64,
}

impl CrossRegionDR {
    pub fn new(primary: &str) -> Self {
        Self {
            primary_region: primary.to_string(),
            regions: HashMap::new(),
            current_ts: 0,
        }
    }

    pub fn add_region(&mut self, config: RegionConfig) {
        self.regions.insert(config.region_name.clone(), config);
    }

    pub fn set_current_time(&mut self, ts: u64) {
        self.current_ts = ts;
    }

    pub fn update_replication(&mut self, region: &str, lsn: u64, ts: u64) {
        if let Some(r) = self.regions.get_mut(region) {
            r.last_replicated_lsn = lsn;
            r.last_replicated_ts = ts;
        }
    }

    /// Check if a region is within its RPO target.
    pub fn is_within_rpo(&self, region: &str) -> bool {
        if let Some(r) = self.regions.get(region) {
            let lag = self.current_ts.saturating_sub(r.last_replicated_ts);
            lag <= r.rpo_target_s
        } else {
            false
        }
    }

    /// Get regions violating RPO.
    pub fn rpo_violations(&self) -> Vec<&str> {
        self.regions.values()
            .filter(|r| {
                let lag = self.current_ts.saturating_sub(r.last_replicated_ts);
                lag > r.rpo_target_s
            })
            .map(|r| r.region_name.as_str())
            .collect()
    }

    pub fn region_count(&self) -> usize {
        self.regions.len()
    }
}

// ── Rolling Upgrade Coordinator ───────────────────────────────────────

/// Upgrade state for a node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpgradeState {
    Pending,
    Draining,
    Upgrading,
    Verifying,
    Completed,
    Failed,
}

/// Node in the rolling upgrade plan.
#[derive(Debug, Clone)]
pub struct UpgradeNode {
    pub node_id: String,
    pub state: UpgradeState,
    pub version_before: String,
    pub version_after: Option<String>,
}

/// Coordinates phased rolling upgrades.
pub struct RollingUpgradeCoordinator {
    nodes: Vec<UpgradeNode>,
    target_version: String,
    max_concurrent: usize,
}

impl RollingUpgradeCoordinator {
    pub fn new(target_version: &str, max_concurrent: usize) -> Self {
        Self {
            nodes: Vec::new(),
            target_version: target_version.to_string(),
            max_concurrent,
        }
    }

    pub fn add_node(&mut self, node_id: &str, current_version: &str) {
        self.nodes.push(UpgradeNode {
            node_id: node_id.to_string(),
            state: UpgradeState::Pending,
            version_before: current_version.to_string(),
            version_after: None,
        });
    }

    /// Get next batch of nodes to upgrade.
    pub fn next_batch(&self) -> Vec<&str> {
        self.nodes.iter()
            .filter(|n| n.state == UpgradeState::Pending)
            .take(self.max_concurrent)
            .map(|n| n.node_id.as_str())
            .collect()
    }

    /// Advance a node to the next state.
    pub fn advance(&mut self, node_id: &str) -> Option<UpgradeState> {
        let node = self.nodes.iter_mut().find(|n| n.node_id == node_id)?;
        node.state = match node.state {
            UpgradeState::Pending => UpgradeState::Draining,
            UpgradeState::Draining => UpgradeState::Upgrading,
            UpgradeState::Upgrading => UpgradeState::Verifying,
            UpgradeState::Verifying => {
                node.version_after = Some(self.target_version.clone());
                UpgradeState::Completed
            }
            other => other,
        };
        Some(node.state)
    }

    pub fn mark_failed(&mut self, node_id: &str) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.node_id == node_id) {
            n.state = UpgradeState::Failed;
        }
    }

    pub fn progress(&self) -> (usize, usize) {
        let done = self.nodes.iter().filter(|n| n.state == UpgradeState::Completed).count();
        (done, self.nodes.len())
    }

    pub fn all_complete(&self) -> bool {
        self.nodes.iter().all(|n| n.state == UpgradeState::Completed)
    }

    pub fn any_failed(&self) -> bool {
        self.nodes.iter().any(|n| n.state == UpgradeState::Failed)
    }
}

// ── Health Probe System ───────────────────────────────────────────────

/// Health check result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

/// A health probe for a component.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub component: String,
    pub status: HealthStatus,
    pub latency_ms: u64,
    pub message: String,
    pub timestamp: u64,
}

/// Health probe system.
pub struct HealthProbe {
    results: HashMap<String, ProbeResult>,
    thresholds: HealthThresholds,
}

/// Configurable thresholds for health determination.
#[derive(Debug, Clone)]
pub struct HealthThresholds {
    pub max_latency_ms: u64,
    pub degraded_latency_ms: u64,
    pub max_age_s: u64,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self { max_latency_ms: 5000, degraded_latency_ms: 1000, max_age_s: 30 }
    }
}

impl HealthProbe {
    pub fn new(thresholds: HealthThresholds) -> Self {
        Self { results: HashMap::new(), thresholds }
    }

    pub fn record(&mut self, component: &str, latency_ms: u64, ok: bool, msg: &str, ts: u64) {
        let status = if !ok {
            HealthStatus::Unhealthy
        } else if latency_ms > self.thresholds.max_latency_ms {
            HealthStatus::Unhealthy
        } else if latency_ms > self.thresholds.degraded_latency_ms {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };
        self.results.insert(component.to_string(), ProbeResult {
            component: component.to_string(),
            status,
            latency_ms,
            message: msg.to_string(),
            timestamp: ts,
        });
    }

    pub fn get_status(&self, component: &str) -> HealthStatus {
        self.results.get(component).map(|r| r.status).unwrap_or(HealthStatus::Unknown)
    }

    /// Overall system health: unhealthy if ANY component is unhealthy.
    pub fn overall_status(&self) -> HealthStatus {
        if self.results.is_empty() { return HealthStatus::Unknown; }
        if self.results.values().any(|r| r.status == HealthStatus::Unhealthy) {
            return HealthStatus::Unhealthy;
        }
        if self.results.values().any(|r| r.status == HealthStatus::Degraded) {
            return HealthStatus::Degraded;
        }
        HealthStatus::Healthy
    }

    /// Components that are not healthy.
    pub fn unhealthy_components(&self) -> Vec<&str> {
        self.results.values()
            .filter(|r| r.status == HealthStatus::Unhealthy)
            .map(|r| r.component.as_str())
            .collect()
    }

    /// Stale probes (older than max_age_s).
    pub fn stale_probes(&self, current_ts: u64) -> Vec<&str> {
        self.results.values()
            .filter(|r| current_ts.saturating_sub(r.timestamp) > self.thresholds.max_age_s)
            .map(|r| r.component.as_str())
            .collect()
    }

    pub fn component_count(&self) -> usize {
        self.results.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failover_chain_selects_best() {
        let mut fc = FailoverChain::new();
        fc.set_leader("node1");
        fc.add_candidate(FailoverCandidate {
            node_id: "node2".to_string(), priority: 1, is_healthy: true,
            last_sync_lsn: 100, region: "us-east".to_string(),
        });
        fc.add_candidate(FailoverCandidate {
            node_id: "node3".to_string(), priority: 2, is_healthy: true,
            last_sync_lsn: 200, region: "us-west".to_string(),
        });
        let best = fc.select_failover().unwrap();
        assert_eq!(best.node_id, "node3"); // highest LSN
    }

    #[test]
    fn failover_chain_failover() {
        let mut fc = FailoverChain::new();
        fc.set_leader("node1");
        fc.add_candidate(FailoverCandidate {
            node_id: "node2".to_string(), priority: 1, is_healthy: true,
            last_sync_lsn: 50, region: "us-east".to_string(),
        });
        let new_leader = fc.failover().unwrap();
        assert_eq!(new_leader, "node2");
        assert_eq!(fc.current_leader(), Some("node2"));
    }

    #[test]
    fn replica_syncer_lag() {
        let mut rs = ReplicaSyncer::new();
        rs.set_primary_lsn(1000);
        rs.add_replica("r1");
        rs.update_replica("r1", 900, 50);
        assert_eq!(rs.lsn_lag("r1"), Some(100));
        assert!(rs.lagging_replicas(30).contains(&"r1"));
        assert!(rs.lagging_replicas(100).is_empty());
    }

    #[test]
    fn cross_region_rpo_check() {
        let mut dr = CrossRegionDR::new("us-east");
        dr.add_region(RegionConfig {
            region_name: "eu-west".to_string(), endpoint: "eu.example.com".to_string(),
            rpo_target_s: 60, rto_target_s: 300, last_replicated_lsn: 0,
            last_replicated_ts: 0,
        });
        dr.set_current_time(100);
        dr.update_replication("eu-west", 50, 50);
        assert!(dr.is_within_rpo("eu-west")); // lag=50 <= 60
        dr.set_current_time(200);
        assert!(!dr.is_within_rpo("eu-west")); // lag=150 > 60
        assert_eq!(dr.rpo_violations().len(), 1);
    }

    #[test]
    fn rolling_upgrade_lifecycle() {
        let mut rc = RollingUpgradeCoordinator::new("2.0.0", 2);
        rc.add_node("n1", "1.0.0");
        rc.add_node("n2", "1.0.0");
        rc.add_node("n3", "1.0.0");
        let batch = rc.next_batch();
        assert_eq!(batch.len(), 2);
        // Advance n1 through all states
        for _ in 0..4 {
            rc.advance("n1");
        }
        assert_eq!(rc.progress(), (1, 3));
        assert!(!rc.all_complete());
    }

    #[test]
    fn health_probe_overall() {
        let mut hp = HealthProbe::new(HealthThresholds::default());
        hp.record("storage", 100, true, "ok", 1);
        hp.record("network", 2000, true, "slow", 1);
        assert_eq!(hp.get_status("storage"), HealthStatus::Healthy);
        assert_eq!(hp.get_status("network"), HealthStatus::Degraded);
        assert_eq!(hp.overall_status(), HealthStatus::Degraded);
        hp.record("raft", 100, false, "down", 1);
        assert_eq!(hp.overall_status(), HealthStatus::Unhealthy);
        assert!(hp.unhealthy_components().contains(&"raft"));
    }
}
