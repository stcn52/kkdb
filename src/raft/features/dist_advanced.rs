// ── src/raft/features/dist_advanced.rs ──
// R22: 分布式系统进阶 — 多Raft组管理 / 跨区域复制 / 动态负载均衡 / 故障自愈

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
// 1. MultiRaftGroupManager — 多Raft组管理
// ═══════════════════════════════════════════════════════════════════════

/// Raft组状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaftGroupState {
    Active,
    Follower,
    Leader,
    Candidate,
    Offline,
}

/// Raft组信息
#[derive(Debug, Clone)]
pub struct RaftGroupInfo {
    pub group_id: u64,
    pub name: String,
    pub state: RaftGroupState,
    pub leader_id: Option<u64>,
    pub members: Vec<u64>,
    pub term: u64,
    pub log_index: u64,
}

/// 多Raft组管理器
pub struct MultiRaftGroupManager {
    groups: HashMap<u64, RaftGroupInfo>,
    next_group_id: u64,
}

impl Default for MultiRaftGroupManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiRaftGroupManager {
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
            next_group_id: 1,
        }
    }

    pub fn create_group(&mut self, name: &str, members: Vec<u64>) -> u64 {
        let id = self.next_group_id;
        self.next_group_id += 1;
        self.groups.insert(
            id,
            RaftGroupInfo {
                group_id: id,
                name: name.to_string(),
                state: RaftGroupState::Active,
                leader_id: members.first().copied(),
                members,
                term: 0,
                log_index: 0,
            },
        );
        id
    }

    pub fn elect_leader(&mut self, group_id: u64, leader_id: u64) {
        if let Some(g) = self.groups.get_mut(&group_id) {
            if g.members.contains(&leader_id) {
                g.leader_id = Some(leader_id);
                g.state = RaftGroupState::Leader;
                g.term += 1;
            }
        }
    }

    pub fn advance_log(&mut self, group_id: u64, entries: u64) {
        if let Some(g) = self.groups.get_mut(&group_id) {
            g.log_index += entries;
        }
    }

    pub fn remove_group(&mut self, group_id: u64) -> bool {
        self.groups.remove(&group_id).is_some()
    }

    pub fn get_group(&self, group_id: u64) -> Option<&RaftGroupInfo> {
        self.groups.get(&group_id)
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn active_groups(&self) -> Vec<u64> {
        self.groups
            .values()
            .filter(|g| g.state != RaftGroupState::Offline)
            .map(|g| g.group_id)
            .collect()
    }

    pub fn leaders(&self) -> Vec<(u64, u64)> {
        self.groups
            .values()
            .filter_map(|g| g.leader_id.map(|lid| (g.group_id, lid)))
            .collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. CrossRegionReplicator — 跨区域复制
// ═══════════════════════════════════════════════════════════════════════

/// 区域信息
#[derive(Debug, Clone)]
pub struct Region {
    pub id: u64,
    pub name: String,
    pub latency_ms: u32,
    pub is_primary: bool,
}

/// 复制状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationState {
    Synced,
    Lagging,
    Disconnected,
    Bootstrapping,
}

/// 复制任务
#[derive(Debug, Clone)]
pub struct ReplicationTask {
    pub source_region: u64,
    pub target_region: u64,
    pub state: ReplicationState,
    pub lag_entries: u64,
    pub bytes_transferred: u64,
}

/// 跨区域复制器
pub struct CrossRegionReplicator {
    regions: Vec<Region>,
    tasks: Vec<ReplicationTask>,
    total_bytes: u64,
}

impl Default for CrossRegionReplicator {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossRegionReplicator {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            tasks: Vec::new(),
            total_bytes: 0,
        }
    }

    pub fn add_region(&mut self, id: u64, name: &str, latency_ms: u32, is_primary: bool) {
        self.regions.push(Region {
            id,
            name: name.to_string(),
            latency_ms,
            is_primary,
        });
    }

    pub fn setup_replication(&mut self, source: u64, target: u64) {
        self.tasks.push(ReplicationTask {
            source_region: source,
            target_region: target,
            state: ReplicationState::Bootstrapping,
            lag_entries: 0,
            bytes_transferred: 0,
        });
    }

    pub fn sync_progress(&mut self, source: u64, target: u64, entries: u64, bytes: u64) {
        if let Some(task) = self
            .tasks
            .iter_mut()
            .find(|t| t.source_region == source && t.target_region == target)
        {
            task.lag_entries = task.lag_entries.saturating_sub(entries);
            task.bytes_transferred += bytes;
            self.total_bytes += bytes;
            if task.lag_entries == 0 {
                task.state = ReplicationState::Synced;
            } else {
                task.state = ReplicationState::Lagging;
            }
        }
    }

    pub fn add_lag(&mut self, source: u64, target: u64, entries: u64) {
        if let Some(task) = self
            .tasks
            .iter_mut()
            .find(|t| t.source_region == source && t.target_region == target)
        {
            task.lag_entries += entries;
            if task.state == ReplicationState::Synced {
                task.state = ReplicationState::Lagging;
            }
        }
    }

    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn synced_tasks(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state == ReplicationState::Synced)
            .count()
    }

    pub fn total_bytes_transferred(&self) -> u64 {
        self.total_bytes
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. DynamicLoadBalancer — 动态负载均衡
// ═══════════════════════════════════════════════════════════════════════

/// 节点负载
#[derive(Debug, Clone)]
pub struct NodeLoad {
    pub node_id: u64,
    pub cpu_pct: f64,
    pub mem_pct: f64,
    pub qps: f64,
    pub active_connections: u32,
    pub weight: f64,
}

/// 负载均衡策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbStrategy {
    RoundRobin,
    LeastConnections,
    WeightedRoundRobin,
    AdaptiveLoad,
}

/// 动态负载均衡器
pub struct DynamicLoadBalancer {
    nodes: Vec<NodeLoad>,
    strategy: LbStrategy,
    rr_index: usize,
    dispatches: u64,
}

impl DynamicLoadBalancer {
    pub fn new(strategy: LbStrategy) -> Self {
        Self {
            nodes: Vec::new(),
            strategy,
            rr_index: 0,
            dispatches: 0,
        }
    }

    pub fn register_node(&mut self, node_id: u64, weight: f64) {
        self.nodes.push(NodeLoad {
            node_id,
            cpu_pct: 0.0,
            mem_pct: 0.0,
            qps: 0.0,
            active_connections: 0,
            weight,
        });
    }

    pub fn update_load(&mut self, node_id: u64, cpu: f64, mem: f64, qps: f64, conns: u32) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.node_id == node_id) {
            n.cpu_pct = cpu;
            n.mem_pct = mem;
            n.qps = qps;
            n.active_connections = conns;
        }
    }

    /// 选择最佳节点
    pub fn select_node(&mut self) -> Option<u64> {
        if self.nodes.is_empty() {
            return None;
        }
        self.dispatches += 1;

        let selected = match self.strategy {
            LbStrategy::RoundRobin => {
                let idx = self.rr_index % self.nodes.len();
                self.rr_index += 1;
                self.nodes[idx].node_id
            }
            LbStrategy::LeastConnections => self
                .nodes
                .iter()
                .min_by_key(|n| n.active_connections)
                .map(|n| n.node_id)
                .unwrap(),
            LbStrategy::WeightedRoundRobin => {
                let idx = self.rr_index % self.nodes.len();
                self.rr_index += 1;
                // Prefer higher weight
                let mut sorted: Vec<&NodeLoad> = self.nodes.iter().collect();
                sorted.sort_by(|a, b| {
                    b.weight
                        .partial_cmp(&a.weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                sorted[idx % sorted.len()].node_id
            }
            LbStrategy::AdaptiveLoad => {
                // Pick lowest combined load score
                self.nodes
                    .iter()
                    .min_by(|a, b| {
                        let score_a =
                            a.cpu_pct * 0.4 + a.mem_pct * 0.3 + a.active_connections as f64 * 0.3;
                        let score_b =
                            b.cpu_pct * 0.4 + b.mem_pct * 0.3 + b.active_connections as f64 * 0.3;
                        score_a
                            .partial_cmp(&score_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|n| n.node_id)
                    .unwrap()
            }
        };

        if let Some(n) = self.nodes.iter_mut().find(|n| n.node_id == selected) {
            n.active_connections += 1;
        }

        Some(selected)
    }

    pub fn release_connection(&mut self, node_id: u64) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.node_id == node_id) {
            n.active_connections = n.active_connections.saturating_sub(1);
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn dispatches(&self) -> u64 {
        self.dispatches
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. SelfHealer — 故障自愈
// ═══════════════════════════════════════════════════════════════════════

/// 故障类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultType {
    NodeCrash,
    NetworkPartition,
    DiskFull,
    SlowQuery,
    Timeout,
    CorruptData,
}

/// 修复动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealAction {
    Restart,
    Failover,
    ScaleOut,
    ThrottleQueries,
    CompactStorage,
    AlertOperator,
}

/// 故障记录
#[derive(Debug, Clone)]
pub struct FaultRecord {
    pub fault_id: u64,
    pub fault_type: FaultType,
    pub node_id: u64,
    pub detected_ms: u64,
    pub resolved: bool,
    pub action_taken: Option<HealAction>,
}

/// 自愈引擎
pub struct SelfHealer {
    faults: Vec<FaultRecord>,
    policies: Vec<(FaultType, HealAction)>,
    next_fault_id: u64,
    healed_count: u64,
}

impl Default for SelfHealer {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfHealer {
    pub fn new() -> Self {
        Self {
            faults: Vec::new(),
            policies: vec![
                (FaultType::NodeCrash, HealAction::Failover),
                (FaultType::NetworkPartition, HealAction::Failover),
                (FaultType::DiskFull, HealAction::CompactStorage),
                (FaultType::SlowQuery, HealAction::ThrottleQueries),
                (FaultType::Timeout, HealAction::Restart),
                (FaultType::CorruptData, HealAction::AlertOperator),
            ],
            next_fault_id: 1,
            healed_count: 0,
        }
    }

    pub fn report_fault(&mut self, fault_type: FaultType, node_id: u64, timestamp_ms: u64) -> u64 {
        let id = self.next_fault_id;
        self.next_fault_id += 1;
        self.faults.push(FaultRecord {
            fault_id: id,
            fault_type,
            node_id,
            detected_ms: timestamp_ms,
            resolved: false,
            action_taken: None,
        });
        id
    }

    /// 自动修复
    pub fn auto_heal(&mut self, fault_id: u64) -> Option<HealAction> {
        let fault = self
            .faults
            .iter_mut()
            .find(|f| f.fault_id == fault_id && !f.resolved)?;
        let action = self
            .policies
            .iter()
            .find(|(ft, _)| *ft == fault.fault_type)
            .map(|(_, a)| *a)?;
        fault.resolved = true;
        fault.action_taken = Some(action);
        self.healed_count += 1;
        Some(action)
    }

    pub fn unresolved_faults(&self) -> Vec<&FaultRecord> {
        self.faults.iter().filter(|f| !f.resolved).collect()
    }

    pub fn fault_count(&self) -> usize {
        self.faults.len()
    }

    pub fn healed_count(&self) -> u64 {
        self.healed_count
    }

    pub fn add_policy(&mut self, fault_type: FaultType, action: HealAction) {
        self.policies.retain(|(ft, _)| *ft != fault_type);
        self.policies.push((fault_type, action));
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multi_raft_group() {
        let mut mgr = MultiRaftGroupManager::new();
        let g1 = mgr.create_group("shard_1", vec![1, 2, 3]);
        let _g2 = mgr.create_group("shard_2", vec![4, 5, 6]);
        assert_eq!(mgr.group_count(), 2);

        mgr.elect_leader(g1, 2);
        let info = mgr.get_group(g1).unwrap();
        assert_eq!(info.leader_id, Some(2));
        assert_eq!(info.term, 1);

        mgr.advance_log(g1, 100);
        assert_eq!(mgr.get_group(g1).unwrap().log_index, 100);
        assert_eq!(mgr.active_groups().len(), 2);
        assert_eq!(mgr.leaders().len(), 2);
    }

    #[test]
    fn test_multi_raft_remove() {
        let mut mgr = MultiRaftGroupManager::new();
        let g1 = mgr.create_group("temp", vec![1]);
        assert!(mgr.remove_group(g1));
        assert_eq!(mgr.group_count(), 0);
    }

    #[test]
    fn test_cross_region_replication() {
        let mut rep = CrossRegionReplicator::new();
        rep.add_region(1, "us-east", 10, true);
        rep.add_region(2, "eu-west", 80, false);
        rep.setup_replication(1, 2);
        assert_eq!(rep.task_count(), 1);

        rep.add_lag(1, 2, 1000);
        rep.sync_progress(1, 2, 1000, 4096);
        assert_eq!(rep.synced_tasks(), 1);
        assert_eq!(rep.total_bytes_transferred(), 4096);
    }

    #[test]
    fn test_cross_region_lagging() {
        let mut rep = CrossRegionReplicator::new();
        rep.add_region(1, "primary", 5, true);
        rep.add_region(2, "secondary", 50, false);
        rep.setup_replication(1, 2);
        rep.add_lag(1, 2, 500);
        rep.sync_progress(1, 2, 100, 1024);
        assert_eq!(rep.synced_tasks(), 0); // still lagging
    }

    #[test]
    fn test_load_balancer_round_robin() {
        let mut lb = DynamicLoadBalancer::new(LbStrategy::RoundRobin);
        lb.register_node(1, 1.0);
        lb.register_node(2, 1.0);
        lb.register_node(3, 1.0);

        let n1 = lb.select_node().unwrap();
        let n2 = lb.select_node().unwrap();
        let n3 = lb.select_node().unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 2);
        assert_eq!(n3, 3);
        assert_eq!(lb.dispatches(), 3);
    }

    #[test]
    fn test_load_balancer_least_connections() {
        let mut lb = DynamicLoadBalancer::new(LbStrategy::LeastConnections);
        lb.register_node(1, 1.0);
        lb.register_node(2, 1.0);
        lb.update_load(1, 50.0, 60.0, 100.0, 10);
        lb.update_load(2, 30.0, 40.0, 50.0, 2);
        let chosen = lb.select_node().unwrap();
        assert_eq!(chosen, 2); // fewer connections
    }

    #[test]
    fn test_self_healer_auto() {
        let mut healer = SelfHealer::new();
        let f1 = healer.report_fault(FaultType::NodeCrash, 3, 1000);
        let f2 = healer.report_fault(FaultType::DiskFull, 5, 2000);
        assert_eq!(healer.unresolved_faults().len(), 2);

        let action = healer.auto_heal(f1).unwrap();
        assert_eq!(action, HealAction::Failover);
        let action2 = healer.auto_heal(f2).unwrap();
        assert_eq!(action2, HealAction::CompactStorage);
        assert_eq!(healer.healed_count(), 2);
        assert!(healer.unresolved_faults().is_empty());
    }

    #[test]
    fn test_self_healer_policy() {
        let mut healer = SelfHealer::new();
        healer.add_policy(FaultType::Timeout, HealAction::ScaleOut);
        let fid = healer.report_fault(FaultType::Timeout, 1, 100);
        let action = healer.auto_heal(fid).unwrap();
        assert_eq!(action, HealAction::ScaleOut);
    }

    #[test]
    fn test_load_balancer_adaptive() {
        let mut lb = DynamicLoadBalancer::new(LbStrategy::AdaptiveLoad);
        lb.register_node(1, 1.0);
        lb.register_node(2, 1.0);
        lb.update_load(1, 90.0, 80.0, 500.0, 50);
        lb.update_load(2, 20.0, 30.0, 100.0, 5);
        let chosen = lb.select_node().unwrap();
        assert_eq!(chosen, 2); // lower combined load
    }
}
