// R18 – Advanced Query Processing:
//   - Materialized view auto-refresh
//   - Distributed query routing
//   - Streaming query result transmission
//   - Dynamic partition pruning
//   - Temporary table lifecycle management
//
// Provides:
//   - `AutoRefreshManager`: schedules and triggers MV refreshes
//   - `QueryRouter`: routes queries to appropriate nodes/shards
//   - `StreamingResult`: chunked result streaming with backpressure
//   - `DynamicPartitionPruner`: runtime partition elimination
//   - `TempTableManager`: tracks temp table creation/cleanup

use std::collections::{HashMap, HashSet, VecDeque};

// ── Materialized View Auto-Refresh ────────────────────────────────────

/// Refresh strategy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RefreshStrategy {
    /// Refresh on a fixed interval.
    Periodic,
    /// Refresh when source data changes.
    OnChange,
    /// Refresh only when explicitly requested.
    Manual,
}

/// Tracked materialized view.
#[derive(Debug, Clone)]
pub struct TrackedView {
    pub name: String,
    pub source_tables: Vec<String>,
    pub strategy: RefreshStrategy,
    pub interval_s: u64,
    pub last_refresh_ts: u64,
    pub refresh_count: u64,
    pub is_stale: bool,
}

/// Manages automatic refresh of materialized views.
pub struct AutoRefreshManager {
    views: HashMap<String, TrackedView>,
}

impl AutoRefreshManager {
    pub fn new() -> Self {
        Self {
            views: HashMap::new(),
        }
    }

    pub fn register_view(
        &mut self,
        name: &str,
        source_tables: Vec<String>,
        strategy: RefreshStrategy,
        interval_s: u64,
    ) {
        self.views.insert(
            name.to_string(),
            TrackedView {
                name: name.to_string(),
                source_tables,
                strategy,
                interval_s,
                last_refresh_ts: 0,
                refresh_count: 0,
                is_stale: true,
            },
        );
    }

    /// Notify that a source table has been modified.
    pub fn notify_table_change(&mut self, table: &str) {
        for view in self.views.values_mut() {
            if view.source_tables.iter().any(|t| t == table) {
                view.is_stale = true;
            }
        }
    }

    /// Check which views need refresh at the given time.
    pub fn views_needing_refresh(&self, current_ts: u64) -> Vec<&str> {
        self.views
            .values()
            .filter(|v| match v.strategy {
                RefreshStrategy::Periodic => {
                    current_ts.saturating_sub(v.last_refresh_ts) >= v.interval_s
                }
                RefreshStrategy::OnChange => v.is_stale,
                RefreshStrategy::Manual => false,
            })
            .map(|v| v.name.as_str())
            .collect()
    }

    /// Mark a view as refreshed.
    pub fn mark_refreshed(&mut self, name: &str, ts: u64) {
        if let Some(v) = self.views.get_mut(name) {
            v.last_refresh_ts = ts;
            v.refresh_count += 1;
            v.is_stale = false;
        }
    }

    pub fn view_count(&self) -> usize {
        self.views.len()
    }

    pub fn stale_count(&self) -> usize {
        self.views.values().filter(|v| v.is_stale).count()
    }
}

// ── Distributed Query Routing ─────────────────────────────────────────

/// Routing target.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteTarget {
    Local,
    Node(String),
    Shard(u32),
    Broadcast,
}

/// Routing rule.
#[derive(Debug, Clone)]
pub struct RoutingRule {
    pub table_pattern: String,
    pub target: RouteTarget,
    pub priority: u32,
}

/// Routes queries to appropriate execution targets.
pub struct QueryRouter {
    rules: Vec<RoutingRule>,
    node_health: HashMap<String, bool>,
    default_target: RouteTarget,
}

impl QueryRouter {
    pub fn new(default: RouteTarget) -> Self {
        Self {
            rules: Vec::new(),
            node_health: HashMap::new(),
            default_target: default,
        }
    }

    pub fn add_rule(&mut self, table_pattern: &str, target: RouteTarget, priority: u32) {
        self.rules.push(RoutingRule {
            table_pattern: table_pattern.to_string(),
            target,
            priority,
        });
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    pub fn set_node_health(&mut self, node: &str, healthy: bool) {
        self.node_health.insert(node.to_string(), healthy);
    }

    /// Route a query based on accessed tables.
    pub fn route(&self, tables: &[&str]) -> RouteTarget {
        for rule in &self.rules {
            for table in tables {
                if Self::matches_pattern(&rule.table_pattern, table) {
                    // Check if target node is healthy
                    if let RouteTarget::Node(ref n) = rule.target {
                        if !self.node_health.get(n).copied().unwrap_or(true) {
                            continue; // skip unhealthy node
                        }
                    }
                    return rule.target.clone();
                }
            }
        }
        self.default_target.clone()
    }

    fn matches_pattern(pattern: &str, table: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        if pattern.ends_with('*') {
            table.starts_with(&pattern[..pattern.len() - 1])
        } else {
            pattern == table
        }
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

// ── Streaming Result Transmission ─────────────────────────────────────

/// A chunk of streaming results.
#[derive(Debug, Clone)]
pub struct ResultChunk {
    pub chunk_id: u64,
    pub rows: Vec<Vec<String>>,
    pub is_last: bool,
}

/// Manages streaming query result delivery with backpressure.
pub struct StreamingResult {
    buffer: VecDeque<ResultChunk>,
    max_buffer_size: usize,
    next_chunk_id: u64,
    total_rows_sent: u64,
    is_complete: bool,
}

impl StreamingResult {
    pub fn new(max_buffer: usize) -> Self {
        Self {
            buffer: VecDeque::new(),
            max_buffer_size: max_buffer,
            next_chunk_id: 0,
            total_rows_sent: 0,
            is_complete: false,
        }
    }

    /// Produce a chunk. Returns false if backpressure (buffer full).
    pub fn produce(&mut self, rows: Vec<Vec<String>>, is_last: bool) -> bool {
        if self.buffer.len() >= self.max_buffer_size {
            return false; // backpressure
        }
        let row_count = rows.len() as u64;
        let chunk = ResultChunk {
            chunk_id: self.next_chunk_id,
            rows,
            is_last,
        };
        self.next_chunk_id += 1;
        self.buffer.push_back(chunk);
        self.total_rows_sent += row_count;
        if is_last {
            self.is_complete = true;
        }
        true
    }

    /// Consume the next chunk.
    pub fn consume(&mut self) -> Option<ResultChunk> {
        self.buffer.pop_front()
    }

    pub fn buffered_chunks(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_backpressured(&self) -> bool {
        self.buffer.len() >= self.max_buffer_size
    }

    pub fn total_rows_sent(&self) -> u64 {
        self.total_rows_sent
    }

    pub fn is_complete(&self) -> bool {
        self.is_complete && self.buffer.is_empty()
    }
}

// ── Dynamic Partition Pruning ─────────────────────────────────────────

/// Runtime partition definition.
#[derive(Debug, Clone)]
pub struct RuntimePartition {
    pub partition_id: u32,
    pub lower_bound: Option<i64>,
    pub upper_bound: Option<i64>,
    pub row_count: u64,
}

/// Prunes partitions at runtime using join-side values.
pub struct DynamicPartitionPruner {
    partitions: Vec<RuntimePartition>,
    pruned_ids: HashSet<u32>,
}

impl DynamicPartitionPruner {
    pub fn new(partitions: Vec<RuntimePartition>) -> Self {
        Self {
            partitions,
            pruned_ids: HashSet::new(),
        }
    }

    /// Prune partitions that cannot contain any of the given values.
    pub fn prune_with_values(&mut self, values: &[i64]) {
        for part in &self.partitions {
            let can_match = values.iter().any(|v| {
                let above_lower = part.lower_bound.map_or(true, |lb| *v >= lb);
                let below_upper = part.upper_bound.map_or(true, |ub| *v <= ub);
                above_lower && below_upper
            });
            if !can_match {
                self.pruned_ids.insert(part.partition_id);
            }
        }
    }

    /// Prune partitions outside a range.
    pub fn prune_with_range(&mut self, lo: i64, hi: i64) {
        for part in &self.partitions {
            let overlaps = part.lower_bound.map_or(true, |lb| lb <= hi)
                && part.upper_bound.map_or(true, |ub| ub >= lo);
            if !overlaps {
                self.pruned_ids.insert(part.partition_id);
            }
        }
    }

    /// Get surviving partition IDs.
    pub fn surviving_partitions(&self) -> Vec<u32> {
        self.partitions
            .iter()
            .filter(|p| !self.pruned_ids.contains(&p.partition_id))
            .map(|p| p.partition_id)
            .collect()
    }

    pub fn pruned_count(&self) -> usize {
        self.pruned_ids.len()
    }

    pub fn total_partitions(&self) -> usize {
        self.partitions.len()
    }

    /// Estimated rows after pruning.
    pub fn estimated_rows(&self) -> u64 {
        self.partitions
            .iter()
            .filter(|p| !self.pruned_ids.contains(&p.partition_id))
            .map(|p| p.row_count)
            .sum()
    }
}

// ── Temporary Table Lifecycle ─────────────────────────────────────────

/// Temp table scope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TempScope {
    Session,
    Transaction,
    Statement,
}

/// A tracked temporary table.
#[derive(Debug, Clone)]
pub struct TempTable {
    pub name: String,
    pub scope: TempScope,
    pub session_id: u64,
    pub created_ts: u64,
    pub row_count: u64,
    pub size_bytes: u64,
}

/// Manages temporary table creation and cleanup.
pub struct TempTableManager {
    tables: HashMap<String, TempTable>,
    max_per_session: usize,
    max_total_bytes: u64,
}

impl TempTableManager {
    pub fn new(max_per_session: usize, max_total_bytes: u64) -> Self {
        Self {
            tables: HashMap::new(),
            max_per_session,
            max_total_bytes,
        }
    }

    /// Create a temp table. Returns Err if limits exceeded.
    pub fn create(
        &mut self,
        name: &str,
        scope: TempScope,
        session_id: u64,
        ts: u64,
    ) -> Result<(), String> {
        let session_count = self
            .tables
            .values()
            .filter(|t| t.session_id == session_id)
            .count();
        if session_count >= self.max_per_session {
            return Err(format!(
                "session {} exceeds max temp tables ({})",
                session_id, self.max_per_session
            ));
        }
        self.tables.insert(
            name.to_string(),
            TempTable {
                name: name.to_string(),
                scope,
                session_id,
                created_ts: ts,
                row_count: 0,
                size_bytes: 0,
            },
        );
        Ok(())
    }

    pub fn update_stats(&mut self, name: &str, rows: u64, bytes: u64) {
        if let Some(t) = self.tables.get_mut(name) {
            t.row_count = rows;
            t.size_bytes = bytes;
        }
    }

    /// Drop a temp table.
    pub fn drop_table(&mut self, name: &str) -> bool {
        self.tables.remove(name).is_some()
    }

    /// Cleanup all temp tables for a session.
    pub fn cleanup_session(&mut self, session_id: u64) -> usize {
        let before = self.tables.len();
        self.tables.retain(|_, t| t.session_id != session_id);
        before - self.tables.len()
    }

    /// Cleanup transaction-scoped temp tables.
    pub fn cleanup_transaction(&mut self, session_id: u64) -> usize {
        let before = self.tables.len();
        self.tables
            .retain(|_, t| !(t.session_id == session_id && t.scope == TempScope::Transaction));
        before - self.tables.len()
    }

    /// Cleanup statement-scoped temp tables.
    pub fn cleanup_statement(&mut self, session_id: u64) -> usize {
        let before = self.tables.len();
        self.tables
            .retain(|_, t| !(t.session_id == session_id && t.scope == TempScope::Statement));
        before - self.tables.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.tables.values().map(|t| t.size_bytes).sum()
    }

    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Check if total usage exceeds limit.
    pub fn is_over_limit(&self) -> bool {
        self.total_bytes() > self.max_total_bytes
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_refresh_periodic() {
        let mut arm = AutoRefreshManager::new();
        arm.register_view(
            "mv_sales",
            vec!["orders".to_string()],
            RefreshStrategy::Periodic,
            60,
        );
        let needing = arm.views_needing_refresh(100);
        assert!(needing.contains(&"mv_sales")); // last=0, interval=60, now=100
        arm.mark_refreshed("mv_sales", 100);
        let needing2 = arm.views_needing_refresh(150);
        assert!(needing2.is_empty()); // 150-100=50 < 60
        let needing3 = arm.views_needing_refresh(161);
        assert!(needing3.contains(&"mv_sales")); // 161-100=61 >= 60
    }

    #[test]
    fn auto_refresh_on_change() {
        let mut arm = AutoRefreshManager::new();
        arm.register_view(
            "mv_totals",
            vec!["sales".to_string()],
            RefreshStrategy::OnChange,
            0,
        );
        arm.mark_refreshed("mv_totals", 10);
        assert_eq!(arm.stale_count(), 0);
        arm.notify_table_change("sales");
        assert_eq!(arm.stale_count(), 1);
        let needing = arm.views_needing_refresh(20);
        assert!(needing.contains(&"mv_totals"));
    }

    #[test]
    fn query_router_table_match() {
        let mut qr = QueryRouter::new(RouteTarget::Local);
        qr.add_rule("orders", RouteTarget::Shard(1), 10);
        qr.add_rule("users", RouteTarget::Node("n2".to_string()), 5);
        assert_eq!(qr.route(&["orders"]), RouteTarget::Shard(1));
        assert_eq!(qr.route(&["unknown"]), RouteTarget::Local);
    }

    #[test]
    fn query_router_unhealthy_skip() {
        let mut qr = QueryRouter::new(RouteTarget::Local);
        qr.add_rule("data", RouteTarget::Node("n1".to_string()), 10);
        qr.set_node_health("n1", false);
        assert_eq!(qr.route(&["data"]), RouteTarget::Local); // skip unhealthy
    }

    #[test]
    fn streaming_result_backpressure() {
        let mut sr = StreamingResult::new(2);
        assert!(sr.produce(vec![vec!["a".to_string()]], false));
        assert!(sr.produce(vec![vec!["b".to_string()]], false));
        assert!(!sr.produce(vec![vec!["c".to_string()]], false)); // backpressured
        let chunk = sr.consume().unwrap();
        assert_eq!(chunk.chunk_id, 0);
        assert!(sr.produce(vec![vec!["c".to_string()]], true)); // now ok
    }

    #[test]
    fn dynamic_partition_pruning_values() {
        let parts = vec![
            RuntimePartition {
                partition_id: 0,
                lower_bound: Some(0),
                upper_bound: Some(99),
                row_count: 100,
            },
            RuntimePartition {
                partition_id: 1,
                lower_bound: Some(100),
                upper_bound: Some(199),
                row_count: 100,
            },
            RuntimePartition {
                partition_id: 2,
                lower_bound: Some(200),
                upper_bound: Some(299),
                row_count: 100,
            },
        ];
        let mut dpp = DynamicPartitionPruner::new(parts);
        dpp.prune_with_values(&[50, 150]);
        let surviving = dpp.surviving_partitions();
        assert_eq!(surviving.len(), 2); // partitions 0 and 1
        assert_eq!(dpp.pruned_count(), 1);
        assert_eq!(dpp.estimated_rows(), 200);
    }

    #[test]
    fn dynamic_partition_pruning_range() {
        let parts = vec![
            RuntimePartition {
                partition_id: 0,
                lower_bound: Some(0),
                upper_bound: Some(99),
                row_count: 50,
            },
            RuntimePartition {
                partition_id: 1,
                lower_bound: Some(100),
                upper_bound: Some(199),
                row_count: 50,
            },
            RuntimePartition {
                partition_id: 2,
                lower_bound: Some(200),
                upper_bound: Some(299),
                row_count: 50,
            },
        ];
        let mut dpp = DynamicPartitionPruner::new(parts);
        dpp.prune_with_range(150, 250);
        let surviving = dpp.surviving_partitions();
        assert_eq!(surviving.len(), 2); // partitions 1 and 2
    }

    #[test]
    fn temp_table_lifecycle() {
        let mut ttm = TempTableManager::new(5, 1_000_000);
        ttm.create("tmp_results", TempScope::Session, 1, 100)
            .unwrap();
        ttm.create("tmp_work", TempScope::Transaction, 1, 100)
            .unwrap();
        ttm.create("tmp_stmt", TempScope::Statement, 1, 100)
            .unwrap();
        assert_eq!(ttm.table_count(), 3);

        let cleaned = ttm.cleanup_statement(1);
        assert_eq!(cleaned, 1);
        assert_eq!(ttm.table_count(), 2);

        let cleaned = ttm.cleanup_transaction(1);
        assert_eq!(cleaned, 1);
        assert_eq!(ttm.table_count(), 1);

        let cleaned = ttm.cleanup_session(1);
        assert_eq!(cleaned, 1);
        assert_eq!(ttm.table_count(), 0);
    }

    #[test]
    fn temp_table_limit() {
        let mut ttm = TempTableManager::new(2, 1_000_000);
        ttm.create("t1", TempScope::Session, 1, 1).unwrap();
        ttm.create("t2", TempScope::Session, 1, 1).unwrap();
        assert!(ttm.create("t3", TempScope::Session, 1, 1).is_err()); // limit reached
    }
}
