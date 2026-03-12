// ── src/raft/features/dist_txn_adv.rs ──
// R20: 分布式事务增强 — Saga模式 / 补偿事务 / 全局死锁检测 / 分布式快照

use std::collections::{HashMap, HashSet};

// ═══════════════════════════════════════════════════════════════════════
// 1. SagaOrchestrator — Saga 分布式事务编排
// ═══════════════════════════════════════════════════════════════════════

/// Saga 步骤状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SagaStepState {
    Pending,
    Running,
    Completed,
    Failed,
    Compensating,
    Compensated,
}

/// Saga 步骤
#[derive(Debug, Clone)]
pub struct SagaStep {
    pub id: u32,
    pub name: String,
    pub service: String,
    pub state: SagaStepState,
    pub has_compensation: bool,
    pub retry_count: u8,
    pub max_retries: u8,
}

impl SagaStep {
    pub fn new(id: u32, name: &str, service: &str, has_compensation: bool) -> Self {
        Self {
            id,
            name: name.to_string(),
            service: service.to_string(),
            state: SagaStepState::Pending,
            has_compensation,
            retry_count: 0,
            max_retries: 3,
        }
    }

    pub fn is_retriable(&self) -> bool {
        self.retry_count < self.max_retries
    }
}

/// Saga 整体状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SagaState {
    Running,
    Completed,
    Compensating,
    Aborted,
}

/// Saga 编排器
pub struct SagaOrchestrator {
    saga_id: u64,
    steps: Vec<SagaStep>,
    state: SagaState,
    current_step: usize,
    completed_sagas: u64,
    aborted_sagas: u64,
}

impl SagaOrchestrator {
    pub fn new(saga_id: u64) -> Self {
        Self {
            saga_id,
            steps: Vec::new(),
            state: SagaState::Running,
            current_step: 0,
            completed_sagas: 0,
            aborted_sagas: 0,
        }
    }

    pub fn add_step(&mut self, name: &str, service: &str, has_compensation: bool) -> u32 {
        let id = self.steps.len() as u32;
        self.steps
            .push(SagaStep::new(id, name, service, has_compensation));
        id
    }

    /// 推进到下一步
    pub fn advance(&mut self) -> SagaState {
        if self.state != SagaState::Running {
            return self.state;
        }
        if self.current_step >= self.steps.len() {
            self.state = SagaState::Completed;
            self.completed_sagas += 1;
            return self.state;
        }
        self.steps[self.current_step].state = SagaStepState::Running;
        self.state
    }

    /// 标记当前步骤完成
    pub fn complete_current(&mut self) {
        if self.current_step < self.steps.len() {
            self.steps[self.current_step].state = SagaStepState::Completed;
            self.current_step += 1;
        }
    }

    /// 当前步骤失败，触发补偿
    pub fn fail_current(&mut self) -> SagaState {
        if self.current_step < self.steps.len() {
            let step = &mut self.steps[self.current_step];
            if step.is_retriable() {
                step.retry_count += 1;
                return self.state;
            }
            step.state = SagaStepState::Failed;
        }
        self.state = SagaState::Compensating;
        self.compensate()
    }

    /// 反向补偿已完成步骤
    fn compensate(&mut self) -> SagaState {
        for i in (0..self.current_step).rev() {
            if self.steps[i].state == SagaStepState::Completed && self.steps[i].has_compensation {
                self.steps[i].state = SagaStepState::Compensating;
                // In real impl: execute compensation action
                self.steps[i].state = SagaStepState::Compensated;
            }
        }
        self.state = SagaState::Aborted;
        self.aborted_sagas += 1;
        self.state
    }

    pub fn saga_id(&self) -> u64 {
        self.saga_id
    }

    pub fn state(&self) -> SagaState {
        self.state
    }

    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    pub fn completed_steps(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.state == SagaStepState::Completed)
            .count()
    }

    pub fn compensated_steps(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.state == SagaStepState::Compensated)
            .count()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. CompensatingTxn — 补偿事务管理
// ═══════════════════════════════════════════════════════════════════════

/// 补偿动作
#[derive(Debug, Clone)]
pub struct CompensationAction {
    pub txn_id: u64,
    pub table: String,
    pub operation: CompensationOp,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompensationOp {
    UndoInsert { rowid: i64 },
    UndoDelete { rowid: i64, data: Vec<String> },
    UndoUpdate { rowid: i64, old_values: Vec<String> },
}

/// 补偿事务日志
pub struct CompensatingTxnLog {
    actions: HashMap<u64, Vec<CompensationAction>>,
    executed: HashSet<u64>,
    total_compensations: u64,
}

impl CompensatingTxnLog {
    pub fn new() -> Self {
        Self {
            actions: HashMap::new(),
            executed: HashSet::new(),
            total_compensations: 0,
        }
    }

    pub fn record(&mut self, txn_id: u64, table: &str, op: CompensationOp) {
        self.actions
            .entry(txn_id)
            .or_default()
            .push(CompensationAction {
                txn_id,
                table: table.to_string(),
                operation: op,
                recorded_at_ms: 0,
            });
    }

    /// 执行补偿（反向重放）
    pub fn compensate(&mut self, txn_id: u64) -> Vec<CompensationAction> {
        if self.executed.contains(&txn_id) {
            return vec![];
        }
        let actions = match self.actions.get(&txn_id) {
            Some(a) => {
                let mut reversed = a.clone();
                reversed.reverse();
                reversed
            }
            None => return vec![],
        };
        self.executed.insert(txn_id);
        self.total_compensations += 1;
        actions
    }

    pub fn has_pending(&self, txn_id: u64) -> bool {
        self.actions.contains_key(&txn_id) && !self.executed.contains(&txn_id)
    }

    pub fn pending_count(&self) -> usize {
        self.actions
            .keys()
            .filter(|id| !self.executed.contains(id))
            .count()
    }

    pub fn total_compensations(&self) -> u64 {
        self.total_compensations
    }

    pub fn action_count(&self, txn_id: u64) -> usize {
        self.actions.get(&txn_id).map(|a| a.len()).unwrap_or(0)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. GlobalDeadlockDetector — 全局死锁检测
// ═══════════════════════════════════════════════════════════════════════

/// 等待图边
#[derive(Debug, Clone)]
pub struct WaitEdge {
    pub waiter_txn: u64,
    pub holder_txn: u64,
    pub resource: String,
    pub node_id: u32,
}

/// 全局死锁检测器 — 跨节点等待图分析
pub struct GlobalDeadlockDetector {
    edges: Vec<WaitEdge>,
    detected_cycles: Vec<Vec<u64>>,
    detection_runs: u64,
    deadlocks_found: u64,
}

impl GlobalDeadlockDetector {
    pub fn new() -> Self {
        Self {
            edges: Vec::new(),
            detected_cycles: Vec::new(),
            detection_runs: 0,
            deadlocks_found: 0,
        }
    }

    pub fn add_edge(&mut self, waiter: u64, holder: u64, resource: &str, node_id: u32) {
        self.edges.push(WaitEdge {
            waiter_txn: waiter,
            holder_txn: holder,
            resource: resource.to_string(),
            node_id,
        });
    }

    pub fn remove_edges_for_txn(&mut self, txn_id: u64) {
        self.edges
            .retain(|e| e.waiter_txn != txn_id && e.holder_txn != txn_id);
    }

    /// 检测死锁环 — DFS 环检测
    pub fn detect(&mut self) -> Vec<Vec<u64>> {
        self.detection_runs += 1;
        let mut adj: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut all_txns: HashSet<u64> = HashSet::new();

        for e in &self.edges {
            adj.entry(e.waiter_txn).or_default().push(e.holder_txn);
            all_txns.insert(e.waiter_txn);
            all_txns.insert(e.holder_txn);
        }

        let mut visited: HashSet<u64> = HashSet::new();
        let mut in_stack: HashSet<u64> = HashSet::new();
        let mut cycles: Vec<Vec<u64>> = Vec::new();

        for &txn in &all_txns {
            if !visited.contains(&txn) {
                let mut path = Vec::new();
                self.dfs(
                    txn,
                    &adj,
                    &mut visited,
                    &mut in_stack,
                    &mut path,
                    &mut cycles,
                );
            }
        }

        self.deadlocks_found += cycles.len() as u64;
        self.detected_cycles = cycles.clone();
        cycles
    }

    fn dfs(
        &self,
        node: u64,
        adj: &HashMap<u64, Vec<u64>>,
        visited: &mut HashSet<u64>,
        in_stack: &mut HashSet<u64>,
        path: &mut Vec<u64>,
        cycles: &mut Vec<Vec<u64>>,
    ) {
        visited.insert(node);
        in_stack.insert(node);
        path.push(node);

        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                if !visited.contains(&next) {
                    self.dfs(next, adj, visited, in_stack, path, cycles);
                } else if in_stack.contains(&next) {
                    // Found cycle
                    let cycle_start = path.iter().position(|&n| n == next).unwrap();
                    let cycle: Vec<u64> = path[cycle_start..].to_vec();
                    cycles.push(cycle);
                }
            }
        }

        path.pop();
        in_stack.remove(&node);
    }

    /// 选择死锁牺牲者（最年轻的事务）
    pub fn pick_victim(&self, cycle: &[u64]) -> Option<u64> {
        cycle.iter().max().copied()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn detection_runs(&self) -> u64 {
        self.detection_runs
    }

    pub fn deadlocks_found(&self) -> u64 {
        self.deadlocks_found
    }

    pub fn last_cycles(&self) -> &[Vec<u64>] {
        &self.detected_cycles
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. DistributedSnapshot — 分布式一致性快照
// ═══════════════════════════════════════════════════════════════════════

/// 节点快照状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPhase {
    Init,
    Marker,
    Recording,
    Complete,
}

/// 单节点快照
#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    pub node_id: u32,
    pub phase: SnapshotPhase,
    pub state_version: u64,
    pub channel_messages: Vec<Vec<u8>>,
}

/// 分布式快照协调器（Chandy-Lamport 风格）
pub struct DistributedSnapshotCoord {
    snapshot_id: u64,
    node_snapshots: HashMap<u32, NodeSnapshot>,
    expected_nodes: HashSet<u32>,
    completed_nodes: HashSet<u32>,
    next_snapshot_id: u64,
    snapshots_taken: u64,
}

impl DistributedSnapshotCoord {
    pub fn new(nodes: Vec<u32>) -> Self {
        let expected: HashSet<u32> = nodes.into_iter().collect();
        Self {
            snapshot_id: 0,
            node_snapshots: HashMap::new(),
            expected_nodes: expected,
            completed_nodes: HashSet::new(),
            next_snapshot_id: 1,
            snapshots_taken: 0,
        }
    }

    /// 发起快照
    pub fn initiate(&mut self) -> u64 {
        self.snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id += 1;
        self.node_snapshots.clear();
        self.completed_nodes.clear();

        for &node_id in &self.expected_nodes {
            self.node_snapshots.insert(
                node_id,
                NodeSnapshot {
                    node_id,
                    phase: SnapshotPhase::Init,
                    state_version: 0,
                    channel_messages: Vec::new(),
                },
            );
        }
        self.snapshot_id
    }

    /// 标记节点开始录制
    pub fn mark_recording(&mut self, node_id: u32, state_version: u64) -> bool {
        if let Some(snap) = self.node_snapshots.get_mut(&node_id) {
            snap.phase = SnapshotPhase::Recording;
            snap.state_version = state_version;
            true
        } else {
            false
        }
    }

    /// 记录通道消息
    pub fn record_channel_message(&mut self, node_id: u32, message: Vec<u8>) {
        if let Some(snap) = self.node_snapshots.get_mut(&node_id) {
            snap.channel_messages.push(message);
        }
    }

    /// 标记节点完成
    pub fn complete_node(&mut self, node_id: u32) -> bool {
        if let Some(snap) = self.node_snapshots.get_mut(&node_id) {
            snap.phase = SnapshotPhase::Complete;
            self.completed_nodes.insert(node_id);
            true
        } else {
            false
        }
    }

    /// 是否全部完成
    pub fn is_complete(&self) -> bool {
        self.completed_nodes == self.expected_nodes
    }

    /// 完成快照
    pub fn finalize(&mut self) -> bool {
        if self.is_complete() {
            self.snapshots_taken += 1;
            true
        } else {
            false
        }
    }

    pub fn snapshot_id(&self) -> u64 {
        self.snapshot_id
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.completed_nodes.len(), self.expected_nodes.len())
    }

    pub fn snapshots_taken(&self) -> u64 {
        self.snapshots_taken
    }

    pub fn node_state_version(&self, node_id: u32) -> Option<u64> {
        self.node_snapshots.get(&node_id).map(|s| s.state_version)
    }

    pub fn channel_message_count(&self) -> usize {
        self.node_snapshots
            .values()
            .map(|s| s.channel_messages.len())
            .sum()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saga_happy_path() {
        let mut saga = SagaOrchestrator::new(1);
        saga.add_step("debit", "account-svc", true);
        saga.add_step("reserve", "inventory-svc", true);
        saga.add_step("ship", "shipping-svc", false);

        assert_eq!(saga.advance(), SagaState::Running);
        saga.complete_current();
        saga.advance();
        saga.complete_current();
        saga.advance();
        saga.complete_current();
        assert_eq!(saga.advance(), SagaState::Completed);
        assert_eq!(saga.completed_steps(), 3);
    }

    #[test]
    fn test_saga_compensation() {
        let mut saga = SagaOrchestrator::new(2);
        saga.add_step("debit", "account", true);
        saga.add_step("reserve", "inventory", true);
        saga.add_step("ship", "shipping", false);

        saga.advance();
        saga.complete_current();
        saga.advance();
        saga.complete_current();
        saga.advance();
        // step 3 fails (after max retries)
        for _ in 0..4 {
            saga.fail_current();
        }
        assert_eq!(saga.state(), SagaState::Aborted);
        assert_eq!(saga.compensated_steps(), 2);
    }

    #[test]
    fn test_saga_retry() {
        let mut saga = SagaOrchestrator::new(3);
        saga.add_step("action", "svc", true);
        saga.advance();
        // First failure: retries
        let state = saga.fail_current();
        assert_eq!(state, SagaState::Running);
    }

    #[test]
    fn test_compensating_txn_log() {
        let mut log = CompensatingTxnLog::new();
        log.record(1, "users", CompensationOp::UndoInsert { rowid: 100 });
        log.record(
            1,
            "orders",
            CompensationOp::UndoUpdate {
                rowid: 200,
                old_values: vec!["old".into()],
            },
        );
        assert_eq!(log.action_count(1), 2);
        assert!(log.has_pending(1));

        let actions = log.compensate(1);
        assert_eq!(actions.len(), 2);
        // Reversed order
        assert!(matches!(
            actions[0].operation,
            CompensationOp::UndoUpdate { .. }
        ));
        assert!(!log.has_pending(1));
        assert_eq!(log.total_compensations(), 1);
    }

    #[test]
    fn test_compensating_txn_idempotent() {
        let mut log = CompensatingTxnLog::new();
        log.record(1, "t", CompensationOp::UndoInsert { rowid: 1 });
        let _ = log.compensate(1);
        let again = log.compensate(1);
        assert!(again.is_empty()); // doesn't re-compensate
    }

    #[test]
    fn test_global_deadlock_detect() {
        let mut dd = GlobalDeadlockDetector::new();
        dd.add_edge(1, 2, "row_10", 0);
        dd.add_edge(2, 3, "row_20", 0);
        dd.add_edge(3, 1, "row_30", 0);

        let cycles = dd.detect();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 3);
        assert_eq!(dd.deadlocks_found(), 1);
    }

    #[test]
    fn test_deadlock_no_cycle() {
        let mut dd = GlobalDeadlockDetector::new();
        dd.add_edge(1, 2, "r1", 0);
        dd.add_edge(2, 3, "r2", 0);
        let cycles = dd.detect();
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_deadlock_victim_selection() {
        let dd = GlobalDeadlockDetector::new();
        let victim = dd.pick_victim(&[100, 200, 150]);
        assert_eq!(victim, Some(200));
    }

    #[test]
    fn test_deadlock_remove_edges() {
        let mut dd = GlobalDeadlockDetector::new();
        dd.add_edge(1, 2, "r1", 0);
        dd.add_edge(2, 1, "r2", 0);
        dd.remove_edges_for_txn(1);
        assert_eq!(dd.edge_count(), 0);
    }

    #[test]
    fn test_distributed_snapshot_workflow() {
        let mut coord = DistributedSnapshotCoord::new(vec![1, 2, 3]);
        let sid = coord.initiate();
        assert!(sid > 0);
        assert!(!coord.is_complete());

        coord.mark_recording(1, 100);
        coord.mark_recording(2, 101);
        coord.mark_recording(3, 102);

        coord.record_channel_message(1, vec![1, 2, 3]);
        coord.record_channel_message(2, vec![4, 5, 6]);

        coord.complete_node(1);
        coord.complete_node(2);
        assert!(!coord.is_complete());
        coord.complete_node(3);
        assert!(coord.is_complete());
        assert!(coord.finalize());
        assert_eq!(coord.snapshots_taken(), 1);
        assert_eq!(coord.channel_message_count(), 2);
    }

    #[test]
    fn test_distributed_snapshot_progress() {
        let mut coord = DistributedSnapshotCoord::new(vec![10, 20]);
        coord.initiate();
        assert_eq!(coord.progress(), (0, 2));
        coord.mark_recording(10, 500);
        coord.complete_node(10);
        assert_eq!(coord.progress(), (1, 2));
        assert_eq!(coord.node_state_version(10), Some(500));
    }
}
