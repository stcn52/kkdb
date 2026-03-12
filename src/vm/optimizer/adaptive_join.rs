// R12 – Adaptive JOIN algorithm selector + Materialized view refresh tracker.
//
// Provides:
//   - `JoinAlgorithm`: enum of supported join strategies
//   - `JoinSelector`: picks optimal join algorithm based on table statistics
//   - `MaterializedViewDef`: definition & refresh tracking for materialized views
//   - `MaterializedViewRegistry`: manages multiple materialized views

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

// ── Join algorithm selection ──────────────────────────────────────────

/// Available join execution strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JoinAlgorithm {
    /// Nested-loop join — O(n·m). Best for small tables or indexed inner.
    NestedLoop,
    /// Hash join — O(n+m). Best when one table fits in memory.
    HashJoin,
    /// Sort-merge join — O(n·log(n) + m·log(m)). Best for pre-sorted data.
    SortMerge,
}

impl std::fmt::Display for JoinAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NestedLoop => write!(f, "NestedLoop"),
            Self::HashJoin => write!(f, "HashJoin"),
            Self::SortMerge => write!(f, "SortMerge"),
        }
    }
}

/// Statistics about a table used for join algorithm selection.
#[derive(Debug, Clone)]
pub struct TableStats {
    pub row_count: usize,
    pub avg_row_bytes: usize,
    pub is_sorted_on_join_key: bool,
    pub has_index_on_join_key: bool,
}

impl TableStats {
    pub fn new(row_count: usize, avg_row_bytes: usize) -> Self {
        Self {
            row_count,
            avg_row_bytes,
            is_sorted_on_join_key: false,
            has_index_on_join_key: false,
        }
    }

    /// Estimated memory footprint for hash table.
    pub fn estimated_hash_bytes(&self) -> usize {
        self.row_count * (self.avg_row_bytes + 24) // row + hash entry overhead
    }
}

/// Adaptive join algorithm selector.
///
/// Given statistics for the left and right sides of a join,
/// picks the most efficient algorithm.
pub struct JoinSelector {
    /// Maximum bytes available for hash table in memory.
    memory_budget: usize,
    /// Threshold (rows) below which nested loop is preferred.
    nested_loop_threshold: usize,
}

impl JoinSelector {
    pub fn new(memory_budget: usize) -> Self {
        Self {
            memory_budget,
            nested_loop_threshold: 100,
        }
    }

    pub fn with_nested_loop_threshold(mut self, threshold: usize) -> Self {
        self.nested_loop_threshold = threshold;
        self
    }

    /// Select the optimal join algorithm for the given table statistics.
    pub fn select(&self, left: &TableStats, right: &TableStats) -> JoinAlgorithm {
        let smaller = std::cmp::min(left.row_count, right.row_count);
        let larger = std::cmp::max(left.row_count, right.row_count);

        // Rule 1: If both tables are small, use nested loop
        if smaller <= self.nested_loop_threshold && larger <= self.nested_loop_threshold * 10 {
            return JoinAlgorithm::NestedLoop;
        }

        // Rule 2: If inner table has an index, use nested loop (index-seek)
        if right.has_index_on_join_key && left.row_count * 10 < right.row_count {
            return JoinAlgorithm::NestedLoop;
        }

        // Rule 3: If both sides are sorted on join key, use sort-merge
        if left.is_sorted_on_join_key && right.is_sorted_on_join_key {
            return JoinAlgorithm::SortMerge;
        }

        // Rule 4: If the smaller side fits in hash memory budget, use hash join
        let build_side = if left.row_count < right.row_count {
            left
        } else {
            right
        };
        if build_side.estimated_hash_bytes() <= self.memory_budget {
            return JoinAlgorithm::HashJoin;
        }

        // Rule 5: Fallback — sort-merge (can spill to disk theoretically)
        JoinAlgorithm::SortMerge
    }

    /// Estimate the cost of a particular join algorithm (in arbitrary units).
    pub fn estimate_cost(&self, algo: JoinAlgorithm, left: &TableStats, right: &TableStats) -> f64 {
        let n = left.row_count as f64;
        let m = right.row_count as f64;
        match algo {
            JoinAlgorithm::NestedLoop => n * m,
            JoinAlgorithm::HashJoin => n + m + n.min(m) * 1.2, // build + probe
            JoinAlgorithm::SortMerge => {
                let sort_cost = if !left.is_sorted_on_join_key {
                    n * n.log2().max(1.0)
                } else {
                    0.0
                } + if !right.is_sorted_on_join_key {
                    m * m.log2().max(1.0)
                } else {
                    0.0
                };
                sort_cost + n + m
            }
        }
    }

    pub fn memory_budget(&self) -> usize {
        self.memory_budget
    }

    pub fn nested_loop_threshold(&self) -> usize {
        self.nested_loop_threshold
    }
}

// ── Materialized View Registry ────────────────────────────────────────

/// Definition of a materialized view.
#[derive(Debug, Clone)]
pub struct MaterializedViewDef {
    /// View name.
    pub name: String,
    /// Underlying SQL query.
    pub query: String,
    /// Source tables referenced in the query.
    pub source_tables: Vec<String>,
    /// Auto-refresh interval (None = manual only).
    pub refresh_interval: Option<Duration>,
    /// Last refresh timestamp.
    pub last_refresh: Option<SystemTime>,
    /// Whether the view is currently stale (source modified since last refresh).
    pub is_stale: bool,
}

impl MaterializedViewDef {
    pub fn new(name: &str, query: &str, source_tables: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            query: query.to_string(),
            source_tables,
            refresh_interval: None,
            is_stale: true, // stale until first refresh
            last_refresh: None,
        }
    }

    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = Some(interval);
        self
    }

    /// Check if the view needs a refresh based on the interval.
    pub fn needs_refresh(&self) -> bool {
        if self.is_stale {
            return true;
        }
        if let (Some(interval), Some(last)) = (self.refresh_interval, self.last_refresh) {
            if let Ok(elapsed) = SystemTime::now().duration_since(last) {
                return elapsed >= interval;
            }
        }
        false
    }

    /// Mark the view as refreshed.
    pub fn mark_refreshed(&mut self) {
        self.last_refresh = Some(SystemTime::now());
        self.is_stale = false;
    }

    /// Mark the view as stale (source data changed).
    pub fn mark_stale(&mut self) {
        self.is_stale = true;
    }
}

/// Registry of materialized views.
pub struct MaterializedViewRegistry {
    views: HashMap<String, MaterializedViewDef>,
}

impl Default for MaterializedViewRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MaterializedViewRegistry {
    pub fn new() -> Self {
        Self {
            views: HashMap::new(),
        }
    }

    /// Register a new materialized view.
    pub fn register(&mut self, view: MaterializedViewDef) -> bool {
        if self.views.contains_key(&view.name) {
            return false; // already exists
        }
        self.views.insert(view.name.clone(), view);
        true
    }

    /// Unregister a materialized view.
    pub fn unregister(&mut self, name: &str) -> bool {
        self.views.remove(name).is_some()
    }

    /// Get a view definition.
    pub fn get(&self, name: &str) -> Option<&MaterializedViewDef> {
        self.views.get(name)
    }

    /// Get a mutable reference to a view.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut MaterializedViewDef> {
        self.views.get_mut(name)
    }

    /// Invalidate all views that depend on a given source table.
    pub fn invalidate_for_table(&mut self, table_name: &str) {
        for view in self.views.values_mut() {
            if view.source_tables.iter().any(|t| t == table_name) {
                view.mark_stale();
            }
        }
    }

    /// Return names of views that need refreshing.
    pub fn stale_views(&self) -> Vec<&str> {
        self.views
            .values()
            .filter(|v| v.needs_refresh())
            .map(|v| v.name.as_str())
            .collect()
    }

    /// Number of registered views.
    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    /// List all view names.
    pub fn names(&self) -> Vec<&str> {
        self.views.keys().map(|s| s.as_str()).collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_selector_small_tables_nested_loop() {
        let sel = JoinSelector::new(1024 * 1024);
        let left = TableStats::new(10, 64);
        let right = TableStats::new(50, 64);
        assert_eq!(sel.select(&left, &right), JoinAlgorithm::NestedLoop);
    }

    #[test]
    fn join_selector_large_fits_in_memory_hash() {
        let sel = JoinSelector::new(10 * 1024 * 1024); // 10MB
        let left = TableStats::new(1000, 100);
        let right = TableStats::new(100_000, 100);
        assert_eq!(sel.select(&left, &right), JoinAlgorithm::HashJoin);
    }

    #[test]
    fn join_selector_sorted_sort_merge() {
        let sel = JoinSelector::new(1024);
        let mut left = TableStats::new(10_000, 100);
        let mut right = TableStats::new(10_000, 100);
        left.is_sorted_on_join_key = true;
        right.is_sorted_on_join_key = true;
        assert_eq!(sel.select(&left, &right), JoinAlgorithm::SortMerge);
    }

    #[test]
    fn join_selector_large_no_memory_sort_merge() {
        let sel = JoinSelector::new(1024); // tiny budget
        let left = TableStats::new(100_000, 200);
        let right = TableStats::new(100_000, 200);
        assert_eq!(sel.select(&left, &right), JoinAlgorithm::SortMerge);
    }

    #[test]
    fn join_selector_indexed_inner_nested_loop() {
        let sel = JoinSelector::new(1024 * 1024);
        let left = TableStats::new(10, 64);
        let mut right = TableStats::new(100_000, 64);
        right.has_index_on_join_key = true;
        assert_eq!(sel.select(&left, &right), JoinAlgorithm::NestedLoop);
    }

    #[test]
    fn join_selector_cost_estimation() {
        let sel = JoinSelector::new(1024 * 1024);
        let left = TableStats::new(1000, 64);
        let right = TableStats::new(1000, 64);
        let nl_cost = sel.estimate_cost(JoinAlgorithm::NestedLoop, &left, &right);
        let hj_cost = sel.estimate_cost(JoinAlgorithm::HashJoin, &left, &right);
        assert!(
            hj_cost < nl_cost,
            "hash join should be cheaper for large tables"
        );
    }

    #[test]
    fn join_algorithm_display() {
        assert_eq!(format!("{}", JoinAlgorithm::NestedLoop), "NestedLoop");
        assert_eq!(format!("{}", JoinAlgorithm::HashJoin), "HashJoin");
        assert_eq!(format!("{}", JoinAlgorithm::SortMerge), "SortMerge");
    }

    #[test]
    fn matview_needs_refresh_when_stale() {
        let view = MaterializedViewDef::new(
            "mv_sales",
            "SELECT sum(amount) FROM sales",
            vec!["sales".to_string()],
        );
        assert!(view.needs_refresh()); // stale by default
    }

    #[test]
    fn matview_not_stale_after_refresh() {
        let mut view = MaterializedViewDef::new(
            "mv_sales",
            "SELECT sum(amount) FROM sales",
            vec!["sales".to_string()],
        );
        view.mark_refreshed();
        assert!(!view.needs_refresh());
    }

    #[test]
    fn matview_stale_after_invalidation() {
        let mut view = MaterializedViewDef::new(
            "mv_sales",
            "SELECT sum(amount) FROM sales",
            vec!["sales".to_string()],
        );
        view.mark_refreshed();
        view.mark_stale();
        assert!(view.needs_refresh());
    }

    #[test]
    fn matview_registry_register_and_get() {
        let mut reg = MaterializedViewRegistry::new();
        let view = MaterializedViewDef::new("mv1", "SELECT 1", vec!["t1".to_string()]);
        assert!(reg.register(view));
        assert_eq!(reg.len(), 1);
        assert!(!reg.register(MaterializedViewDef::new("mv1", "SELECT 2", vec![])));
        assert!(reg.get("mv1").is_some());
    }

    #[test]
    fn matview_registry_invalidate_for_table() {
        let mut reg = MaterializedViewRegistry::new();
        let mut v1 = MaterializedViewDef::new("mv1", "SELECT * FROM t1", vec!["t1".to_string()]);
        v1.mark_refreshed();
        let mut v2 = MaterializedViewDef::new("mv2", "SELECT * FROM t2", vec!["t2".to_string()]);
        v2.mark_refreshed();
        reg.register(v1);
        reg.register(v2);
        assert!(reg.stale_views().is_empty());

        reg.invalidate_for_table("t1");
        let stale = reg.stale_views();
        assert_eq!(stale.len(), 1);
        assert!(stale.contains(&"mv1"));
    }

    #[test]
    fn matview_registry_unregister() {
        let mut reg = MaterializedViewRegistry::new();
        reg.register(MaterializedViewDef::new("mv1", "SELECT 1", vec![]));
        assert!(reg.unregister("mv1"));
        assert!(!reg.unregister("mv1"));
        assert!(reg.is_empty());
    }
}
