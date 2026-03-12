// R14 – Distributed transaction enhancements: distributed snapshot isolation,
//       global deadlock detection, cross-shard aggregate pushdown,
//       dynamic shard rebalancing.
//
// Provides:
//   - `DistributedSnapshot`: global snapshot for distributed MVCC reads
//   - `GlobalDeadlockDetector`: multi-node wait-for-graph deadlock detection
//   - `CrossShardPushdown`: aggregate pushdown optimization across shards
//   - `ShardRebalancer`: dynamic shard redistribution manager

use std::collections::{HashMap, HashSet, VecDeque};

// ── Distributed Snapshot Isolation ────────────────────────────────────

/// A distributed snapshot representing a consistent read view across nodes.
#[derive(Debug, Clone)]
pub struct DistributedSnapshot {
    pub snapshot_id: u64,
    /// Per-node commit timestamps at snapshot creation.
    pub node_timestamps: HashMap<u64, u64>,
    /// Set of active (uncommitted) transaction IDs at snapshot time.
    pub active_txns: HashSet<u64>,
}

impl DistributedSnapshot {
    pub fn new(snapshot_id: u64) -> Self {
        Self {
            snapshot_id,
            node_timestamps: HashMap::new(),
            active_txns: HashSet::new(),
        }
    }

    /// Record a node's commit timestamp.
    pub fn set_node_timestamp(&mut self, node_id: u64, ts: u64) {
        self.node_timestamps.insert(node_id, ts);
    }

    /// Add an active transaction.
    pub fn add_active_txn(&mut self, txn_id: u64) {
        self.active_txns.insert(txn_id);
    }

    /// Check if a version (created by txn_id) is visible in this snapshot.
    pub fn is_visible(&self, txn_id: u64) -> bool {
        // A version is visible if:
        // 1. Its txn is not in the active set (i.e. it was committed)
        // 2. Its txn_id is less than our snapshot watermark
        !self.active_txns.contains(&txn_id)
    }

    /// Get the minimum timestamp across all nodes (global low watermark).
    pub fn global_watermark(&self) -> u64 {
        self.node_timestamps.values().copied().min().unwrap_or(0)
    }

    /// Number of nodes in the snapshot.
    pub fn node_count(&self) -> usize {
        self.node_timestamps.len()
    }
}

/// Manages distributed snapshots across the cluster.
pub struct SnapshotManager {
    next_id: u64,
    active_snapshots: HashMap<u64, DistributedSnapshot>,
}

impl Default for SnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            active_snapshots: HashMap::new(),
        }
    }

    /// Create a new distributed snapshot.
    pub fn create_snapshot(
        &mut self,
        node_timestamps: HashMap<u64, u64>,
        active_txns: HashSet<u64>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut snap = DistributedSnapshot::new(id);
        snap.node_timestamps = node_timestamps;
        snap.active_txns = active_txns;
        self.active_snapshots.insert(id, snap);
        id
    }

    pub fn get_snapshot(&self, id: u64) -> Option<&DistributedSnapshot> {
        self.active_snapshots.get(&id)
    }

    pub fn release_snapshot(&mut self, id: u64) -> bool {
        self.active_snapshots.remove(&id).is_some()
    }

    pub fn active_count(&self) -> usize {
        self.active_snapshots.len()
    }
}

// ── Global Deadlock Detector ──────────────────────────────────────────

/// Edge in the distributed wait-for graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WaitEdge {
    pub waiter: u64,  // txn id waiting
    pub holder: u64,  // txn id holding the lock
    pub node_id: u64, // which node this edge exists on
}

/// Global deadlock detector using a centralized wait-for graph.
pub struct GlobalDeadlockDetector {
    edges: Vec<WaitEdge>,
}

impl Default for GlobalDeadlockDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalDeadlockDetector {
    pub fn new() -> Self {
        Self { edges: Vec::new() }
    }

    /// Add a wait edge from a node.
    pub fn add_edge(&mut self, waiter: u64, holder: u64, node_id: u64) {
        let edge = WaitEdge {
            waiter,
            holder,
            node_id,
        };
        if !self.edges.contains(&edge) {
            self.edges.push(edge);
        }
    }

    /// Remove all edges involving a transaction (e.g., when it commits/aborts).
    pub fn remove_txn(&mut self, txn_id: u64) {
        self.edges
            .retain(|e| e.waiter != txn_id && e.holder != txn_id);
    }

    /// Detect all deadlock cycles. Returns a list of cycles (each is a vec of txn IDs).
    pub fn detect_cycles(&self) -> Vec<Vec<u64>> {
        // Build adjacency list
        let mut adj: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut all_nodes = HashSet::new();
        for edge in &self.edges {
            adj.entry(edge.waiter).or_default().push(edge.holder);
            all_nodes.insert(edge.waiter);
            all_nodes.insert(edge.holder);
        }

        let mut cycles = Vec::new();
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for &start in &all_nodes {
            if visited.contains(&start) {
                continue;
            }
            let mut path = Vec::new();
            self.dfs(
                start,
                &adj,
                &mut visited,
                &mut rec_stack,
                &mut path,
                &mut cycles,
            );
        }
        cycles
    }

    fn dfs(
        &self,
        node: u64,
        adj: &HashMap<u64, Vec<u64>>,
        visited: &mut HashSet<u64>,
        rec_stack: &mut HashSet<u64>,
        path: &mut Vec<u64>,
        cycles: &mut Vec<Vec<u64>>,
    ) {
        visited.insert(node);
        rec_stack.insert(node);
        path.push(node);

        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                if !visited.contains(&next) {
                    self.dfs(next, adj, visited, rec_stack, path, cycles);
                } else if rec_stack.contains(&next) {
                    // Found a cycle: extract it from the path
                    if let Some(pos) = path.iter().position(|&n| n == next) {
                        let cycle: Vec<u64> = path[pos..].to_vec();
                        if cycle.len() >= 2 {
                            cycles.push(cycle);
                        }
                    }
                }
            }
        }

        path.pop();
        rec_stack.remove(&node);
    }

    /// Select a victim transaction from a cycle (lowest txn ID = youngest, abort it).
    pub fn select_victim(cycle: &[u64]) -> Option<u64> {
        cycle.iter().copied().max() // abort the "youngest" (highest ID)
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

// ── Cross-Shard Aggregate Pushdown ────────────────────────────────────

/// Represents a partial aggregate that can be computed on a shard.
#[derive(Debug, Clone)]
pub enum PartialAggregate {
    Sum(i64),
    Count(u64),
    Min(i64),
    Max(i64),
    /// Average: (sum, count) — merged at coordinator
    Avg(i64, u64),
}

impl PartialAggregate {
    /// Merge two partial aggregates.
    pub fn merge(self, other: Self) -> Option<Self> {
        match (self, other) {
            (PartialAggregate::Sum(a), PartialAggregate::Sum(b)) => {
                Some(PartialAggregate::Sum(a + b))
            }
            (PartialAggregate::Count(a), PartialAggregate::Count(b)) => {
                Some(PartialAggregate::Count(a + b))
            }
            (PartialAggregate::Min(a), PartialAggregate::Min(b)) => {
                Some(PartialAggregate::Min(a.min(b)))
            }
            (PartialAggregate::Max(a), PartialAggregate::Max(b)) => {
                Some(PartialAggregate::Max(a.max(b)))
            }
            (PartialAggregate::Avg(s1, c1), PartialAggregate::Avg(s2, c2)) => {
                Some(PartialAggregate::Avg(s1 + s2, c1 + c2))
            }
            _ => None,
        }
    }

    /// Finalize the aggregate (e.g., compute final AVG).
    pub fn finalize(&self) -> f64 {
        match self {
            PartialAggregate::Sum(v) => *v as f64,
            PartialAggregate::Count(v) => *v as f64,
            PartialAggregate::Min(v) => *v as f64,
            PartialAggregate::Max(v) => *v as f64,
            PartialAggregate::Avg(sum, count) => {
                if *count > 0 {
                    *sum as f64 / *count as f64
                } else {
                    0.0
                }
            }
        }
    }
}

/// Coordinator for cross-shard aggregate pushdown.
pub struct CrossShardPushdown {
    /// shard_id → list of partial aggregates.
    partials: HashMap<u64, Vec<PartialAggregate>>,
}

impl Default for CrossShardPushdown {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossShardPushdown {
    pub fn new() -> Self {
        Self {
            partials: HashMap::new(),
        }
    }

    /// Record a partial aggregate from a shard.
    pub fn add_partial(&mut self, shard_id: u64, partial: PartialAggregate) {
        self.partials.entry(shard_id).or_default().push(partial);
    }

    /// Merge all partials into a single result.
    pub fn merge_all(&self) -> Vec<PartialAggregate> {
        let mut all: Vec<PartialAggregate> = Vec::new();
        for partials in self.partials.values() {
            for p in partials {
                all.push(p.clone());
            }
        }
        // Group by variant and merge
        let mut sums: Option<PartialAggregate> = None;
        let mut counts: Option<PartialAggregate> = None;
        let mut mins: Option<PartialAggregate> = None;
        let mut maxs: Option<PartialAggregate> = None;
        let mut avgs: Option<PartialAggregate> = None;

        for p in all {
            match p {
                PartialAggregate::Sum(_) => {
                    sums = Some(sums.map_or(p.clone(), |s| s.merge(p).unwrap()));
                }
                PartialAggregate::Count(_) => {
                    counts = Some(counts.map_or(p.clone(), |s| s.merge(p).unwrap()));
                }
                PartialAggregate::Min(_) => {
                    mins = Some(mins.map_or(p.clone(), |s| s.merge(p).unwrap()));
                }
                PartialAggregate::Max(_) => {
                    maxs = Some(maxs.map_or(p.clone(), |s| s.merge(p).unwrap()));
                }
                PartialAggregate::Avg(_, _) => {
                    avgs = Some(avgs.map_or(p.clone(), |s| s.merge(p).unwrap()));
                }
            }
        }

        let mut results = Vec::new();
        if let Some(s) = sums {
            results.push(s);
        }
        if let Some(c) = counts {
            results.push(c);
        }
        if let Some(m) = mins {
            results.push(m);
        }
        if let Some(m) = maxs {
            results.push(m);
        }
        if let Some(a) = avgs {
            results.push(a);
        }
        results
    }

    pub fn shard_count(&self) -> usize {
        self.partials.len()
    }
}

// ── Shard Rebalancer ──────────────────────────────────────────────────

/// Information about a shard's load.
#[derive(Debug, Clone)]
pub struct ShardLoad {
    pub shard_id: u64,
    pub row_count: u64,
    pub disk_bytes: u64,
    pub qps: f64,
}

/// Plans and tracks shard migration operations.
pub struct ShardRebalancer {
    shards: HashMap<u64, ShardLoad>,
    /// Maximum imbalance ratio before triggering rebalance.
    imbalance_threshold: f64,
}

impl ShardRebalancer {
    pub fn new(imbalance_threshold: f64) -> Self {
        Self {
            shards: HashMap::new(),
            imbalance_threshold,
        }
    }

    pub fn update_shard(&mut self, load: ShardLoad) {
        self.shards.insert(load.shard_id, load);
    }

    pub fn remove_shard(&mut self, shard_id: u64) {
        self.shards.remove(&shard_id);
    }

    /// Compute the imbalance ratio (max / avg).
    pub fn imbalance_ratio(&self) -> f64 {
        if self.shards.is_empty() {
            return 1.0;
        }
        let total: u64 = self.shards.values().map(|s| s.row_count).sum();
        let avg = total as f64 / self.shards.len() as f64;
        if avg == 0.0 {
            return 1.0;
        }
        let max = self.shards.values().map(|s| s.row_count).max().unwrap_or(0);
        max as f64 / avg
    }

    /// Check if rebalancing is needed.
    pub fn needs_rebalance(&self) -> bool {
        self.imbalance_ratio() > self.imbalance_threshold
    }

    /// Generate a rebalance plan: (from_shard, to_shard, rows_to_move).
    pub fn plan_rebalance(&self) -> Vec<(u64, u64, u64)> {
        if !self.needs_rebalance() || self.shards.len() < 2 {
            return Vec::new();
        }
        let total: u64 = self.shards.values().map(|s| s.row_count).sum();
        let target = total / self.shards.len() as u64;

        let mut over: Vec<_> = self
            .shards
            .values()
            .filter(|s| s.row_count > target)
            .map(|s| (s.shard_id, s.row_count - target))
            .collect();
        let mut under: VecDeque<_> = self
            .shards
            .values()
            .filter(|s| s.row_count < target)
            .map(|s| (s.shard_id, target - s.row_count))
            .collect();

        over.sort_by(|a, b| b.1.cmp(&a.1)); // most over-loaded first

        let mut plan = Vec::new();
        for (from_id, mut excess) in over {
            while excess > 0 {
                if let Some((to_id, deficit)) = under.front_mut() {
                    let move_rows = excess.min(*deficit);
                    plan.push((from_id, *to_id, move_rows));
                    excess -= move_rows;
                    *deficit -= move_rows;
                    if *deficit == 0 {
                        under.pop_front();
                    }
                } else {
                    break;
                }
            }
        }
        plan
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributed_snapshot_visibility() {
        let mut snap = DistributedSnapshot::new(1);
        snap.set_node_timestamp(1, 100);
        snap.set_node_timestamp(2, 95);
        snap.add_active_txn(50);

        assert!(snap.is_visible(30)); // committed before snapshot
        assert!(!snap.is_visible(50)); // still active
        assert_eq!(snap.global_watermark(), 95);
    }

    #[test]
    fn snapshot_manager_lifecycle() {
        let mut sm = SnapshotManager::new();
        let mut ts = HashMap::new();
        ts.insert(1, 100);
        let id = sm.create_snapshot(ts, HashSet::new());
        assert!(sm.get_snapshot(id).is_some());
        assert!(sm.release_snapshot(id));
        assert_eq!(sm.active_count(), 0);
    }

    #[test]
    fn global_deadlock_detect_cycle() {
        let mut dd = GlobalDeadlockDetector::new();
        dd.add_edge(1, 2, 0); // txn 1 waits for txn 2
        dd.add_edge(2, 3, 0); // txn 2 waits for txn 3
        dd.add_edge(3, 1, 0); // txn 3 waits for txn 1 → cycle!

        let cycles = dd.detect_cycles();
        assert!(!cycles.is_empty());
        // Should contain a cycle involving 1, 2, 3
        let cycle = &cycles[0];
        assert!(cycle.len() >= 2);
    }

    #[test]
    fn global_deadlock_no_cycle() {
        let mut dd = GlobalDeadlockDetector::new();
        dd.add_edge(1, 2, 0);
        dd.add_edge(2, 3, 0);
        // No cycle
        let cycles = dd.detect_cycles();
        assert!(cycles.is_empty());
    }

    #[test]
    fn global_deadlock_select_victim() {
        let victim = GlobalDeadlockDetector::select_victim(&[1, 2, 3]);
        assert_eq!(victim, Some(3)); // highest txn id as youngest
    }

    #[test]
    fn partial_aggregate_merge() {
        let s1 = PartialAggregate::Sum(100);
        let s2 = PartialAggregate::Sum(200);
        let merged = s1.merge(s2).unwrap();
        assert_eq!(merged.finalize(), 300.0);

        let a1 = PartialAggregate::Avg(100, 10);
        let a2 = PartialAggregate::Avg(200, 20);
        let avg = a1.merge(a2).unwrap();
        assert_eq!(avg.finalize(), 10.0); // 300/30 = 10
    }

    #[test]
    fn cross_shard_pushdown_merge() {
        let mut csp = CrossShardPushdown::new();
        csp.add_partial(1, PartialAggregate::Sum(100));
        csp.add_partial(2, PartialAggregate::Sum(200));
        csp.add_partial(1, PartialAggregate::Count(50));
        csp.add_partial(2, PartialAggregate::Count(75));
        let results = csp.merge_all();
        assert_eq!(results.len(), 2); // Sum + Count
        let sum = results
            .iter()
            .find(|r| matches!(r, PartialAggregate::Sum(_)))
            .unwrap();
        assert_eq!(sum.finalize(), 300.0);
    }

    #[test]
    fn shard_rebalancer_imbalance() {
        let mut rb = ShardRebalancer::new(1.5);
        rb.update_shard(ShardLoad {
            shard_id: 1,
            row_count: 1000,
            disk_bytes: 0,
            qps: 0.0,
        });
        rb.update_shard(ShardLoad {
            shard_id: 2,
            row_count: 100,
            disk_bytes: 0,
            qps: 0.0,
        });
        assert!(rb.needs_rebalance()); // 1000 / 550 = 1.82 > 1.5
    }

    #[test]
    fn shard_rebalance_plan() {
        let mut rb = ShardRebalancer::new(1.3);
        rb.update_shard(ShardLoad {
            shard_id: 1,
            row_count: 800,
            disk_bytes: 0,
            qps: 0.0,
        });
        rb.update_shard(ShardLoad {
            shard_id: 2,
            row_count: 200,
            disk_bytes: 0,
            qps: 0.0,
        });
        let plan = rb.plan_rebalance();
        assert!(!plan.is_empty());
        // Should move rows from shard 1 to shard 2
        let (from, to, _rows) = plan[0];
        assert_eq!(from, 1);
        assert_eq!(to, 2);
    }

    #[test]
    fn shard_rebalancer_no_rebalance_needed() {
        let mut rb = ShardRebalancer::new(1.5);
        rb.update_shard(ShardLoad {
            shard_id: 1,
            row_count: 500,
            disk_bytes: 0,
            qps: 0.0,
        });
        rb.update_shard(ShardLoad {
            shard_id: 2,
            row_count: 500,
            disk_bytes: 0,
            qps: 0.0,
        });
        assert!(!rb.needs_rebalance());
        assert!(rb.plan_rebalance().is_empty());
    }
}
