// R15 – Query execution engine advanced: window function streaming,
//       sort spill to disk, semi/anti join optimization, adaptive parallelism.
//
// Provides:
//   - `StreamingWindow`: row-level window function evaluation without buffering
//   - `SortSpillManager`: tracks memory budget, decides when to spill to disk
//   - `SemiAntiJoinOptimizer`: rewrite planner for semi/anti joins
//   - `AdaptiveParallelism`: runtime parallel degree control

use std::collections::{HashMap, HashSet, VecDeque};

// ── Streaming Window ──────────────────────────────────────────────────

/// Window frame boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameBound {
    UnboundedPreceding,
    Preceding(usize),
    CurrentRow,
    Following(usize),
    UnboundedFollowing,
}

/// A window function definition.
#[derive(Debug, Clone)]
pub struct WindowDef {
    pub func_name: String,
    pub partition_cols: Vec<usize>,
    pub order_col: Option<usize>,
    pub start: FrameBound,
    pub end: FrameBound,
}

impl WindowDef {
    pub fn new(func_name: &str) -> Self {
        Self {
            func_name: func_name.to_string(),
            partition_cols: Vec::new(),
            order_col: None,
            start: FrameBound::UnboundedPreceding,
            end: FrameBound::CurrentRow,
        }
    }

    pub fn with_partition(mut self, cols: Vec<usize>) -> Self {
        self.partition_cols = cols;
        self
    }

    pub fn with_order(mut self, col: usize) -> Self {
        self.order_col = Some(col);
        self
    }

    pub fn with_frame(mut self, start: FrameBound, end: FrameBound) -> Self {
        self.start = start;
        self.end = end;
        self
    }
}

/// Streaming window evaluator — processes rows one at a time.
pub struct StreamingWindow {
    def: WindowDef,
    /// Running accumulator for aggregations (sum, count, etc.)
    running_sum: i64,
    running_count: usize,
    /// Partition key → accumulated state.
    partition_states: HashMap<Vec<i64>, (i64, usize)>,
    /// Total rows processed.
    total_rows: usize,
}

impl StreamingWindow {
    pub fn new(def: WindowDef) -> Self {
        Self {
            def,
            running_sum: 0,
            running_count: 0,
            partition_states: HashMap::new(),
            total_rows: 0,
        }
    }

    /// Feed a row and get the current window function value.
    /// `row` is the full row, and value is extracted from the order column.
    pub fn process_row(&mut self, row: &[i64]) -> i64 {
        self.total_rows += 1;

        let partition_key: Vec<i64> = self
            .def
            .partition_cols
            .iter()
            .filter_map(|&c| row.get(c).copied())
            .collect();

        let value = self
            .def
            .order_col
            .and_then(|c| row.get(c).copied())
            .unwrap_or(1);

        let state = self.partition_states.entry(partition_key).or_insert((0, 0));
        state.0 += value;
        state.1 += 1;

        match self.def.func_name.as_str() {
            "sum" => {
                self.running_sum += value;
                state.0
            }
            "count" => {
                self.running_count += 1;
                state.1 as i64
            }
            "row_number" => state.1 as i64,
            "avg" => {
                if state.1 > 0 {
                    state.0 / state.1 as i64
                } else {
                    0
                }
            }
            _ => state.0,
        }
    }

    pub fn reset(&mut self) {
        self.running_sum = 0;
        self.running_count = 0;
        self.partition_states.clear();
        self.total_rows = 0;
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    pub fn func_name(&self) -> &str {
        &self.def.func_name
    }
}

// ── Sort Spill Manager ────────────────────────────────────────────────

/// Policies for sort spill decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpillPolicy {
    /// Never spill (always in-memory).
    Never,
    /// Spill when memory exceeds threshold.
    ThresholdBased,
    /// Adaptive: spill based on available memory pressure.
    Adaptive,
}

/// Sort run stored on "disk" (simulated in-memory for the struct).
#[derive(Debug, Clone)]
pub struct SortRun {
    pub run_id: u32,
    pub row_count: usize,
    pub byte_size: usize,
    pub is_spilled: bool,
}

/// Manages memory budget and spill decisions for large sort operations.
pub struct SortSpillManager {
    policy: SpillPolicy,
    memory_budget: usize,
    memory_used: usize,
    runs: Vec<SortRun>,
    next_run_id: u32,
    spill_count: usize,
}

impl SortSpillManager {
    pub fn new(policy: SpillPolicy, memory_budget: usize) -> Self {
        Self {
            policy,
            memory_budget,
            memory_used: 0,
            runs: Vec::new(),
            next_run_id: 0,
            spill_count: 0,
        }
    }

    /// Record a sort run. Returns true if the run was spilled.
    pub fn add_run(&mut self, row_count: usize, byte_size: usize) -> bool {
        let should_spill = match self.policy {
            SpillPolicy::Never => false,
            SpillPolicy::ThresholdBased => self.memory_used + byte_size > self.memory_budget,
            SpillPolicy::Adaptive => {
                let pressure = (self.memory_used + byte_size) as f64 / self.memory_budget as f64;
                pressure > 0.8
            }
        };

        let run = SortRun {
            run_id: self.next_run_id,
            row_count,
            byte_size,
            is_spilled: should_spill,
        };
        self.next_run_id += 1;

        if should_spill {
            self.spill_count += 1;
        } else {
            self.memory_used += byte_size;
        }

        self.runs.push(run);
        should_spill
    }

    /// Merge runs: simulate k-way merge of sorted runs.
    pub fn merge_runs(&self) -> usize {
        self.runs.iter().map(|r| r.row_count).sum()
    }

    pub fn spill_count(&self) -> usize {
        self.spill_count
    }

    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    pub fn memory_used(&self) -> usize {
        self.memory_used
    }

    pub fn policy(&self) -> SpillPolicy {
        self.policy
    }
}

// ── Semi/Anti Join Optimizer ──────────────────────────────────────────

/// Join type classification.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Semi,
    Anti,
    Cross,
}

/// A semi/anti join rewrite candidate.
#[derive(Debug, Clone)]
pub struct JoinCandidate {
    pub original_kind: JoinKind,
    pub rewritten_kind: JoinKind,
    pub left_table: String,
    pub right_table: String,
    pub join_col: String,
    pub estimated_reduction: f64,
}

/// Optimizer that detects and rewrites semi/anti join opportunities.
pub struct SemiAntiJoinOptimizer {
    rewrites: Vec<JoinCandidate>,
}

impl SemiAntiJoinOptimizer {
    pub fn new() -> Self {
        Self {
            rewrites: Vec::new(),
        }
    }

    /// Check if an EXISTS subquery can be rewritten to a semi join.
    pub fn rewrite_exists_to_semi(
        &mut self,
        left_table: &str,
        right_table: &str,
        join_col: &str,
        right_row_count: usize,
    ) -> JoinCandidate {
        let reduction = if right_row_count > 0 {
            1.0 / right_row_count as f64
        } else {
            0.0
        };
        let candidate = JoinCandidate {
            original_kind: JoinKind::Inner,
            rewritten_kind: JoinKind::Semi,
            left_table: left_table.to_string(),
            right_table: right_table.to_string(),
            join_col: join_col.to_string(),
            estimated_reduction: reduction,
        };
        self.rewrites.push(candidate.clone());
        candidate
    }

    /// Check if a NOT EXISTS subquery can be rewritten to an anti join.
    pub fn rewrite_not_exists_to_anti(
        &mut self,
        left_table: &str,
        right_table: &str,
        join_col: &str,
    ) -> JoinCandidate {
        let candidate = JoinCandidate {
            original_kind: JoinKind::Left,
            rewritten_kind: JoinKind::Anti,
            left_table: left_table.to_string(),
            right_table: right_table.to_string(),
            join_col: join_col.to_string(),
            estimated_reduction: 0.5,
        };
        self.rewrites.push(candidate.clone());
        candidate
    }

    /// Execute a semi join: return left rows that have at least one match in right.
    pub fn execute_semi(left_keys: &[i64], right_keys: &[i64]) -> Vec<usize> {
        let right_set: HashSet<i64> = right_keys.iter().copied().collect();
        left_keys
            .iter()
            .enumerate()
            .filter(|(_, k)| right_set.contains(k))
            .map(|(i, _)| i)
            .collect()
    }

    /// Execute an anti join: return left rows that have no match in right.
    pub fn execute_anti(left_keys: &[i64], right_keys: &[i64]) -> Vec<usize> {
        let right_set: HashSet<i64> = right_keys.iter().copied().collect();
        left_keys
            .iter()
            .enumerate()
            .filter(|(_, k)| !right_set.contains(k))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn rewrite_count(&self) -> usize {
        self.rewrites.len()
    }
}

// ── Adaptive Parallelism ──────────────────────────────────────────────

/// Runtime statistics for adjusting parallelism.
#[derive(Debug, Clone)]
pub struct ParallelismStats {
    pub cpu_utilization: f64, // 0.0 - 1.0
    pub io_wait_ratio: f64,   // 0.0 - 1.0
    pub queue_depth: usize,
    pub active_queries: usize,
}

/// Controls adaptive parallelism degree at runtime.
pub struct AdaptiveParallelism {
    min_degree: usize,
    max_degree: usize,
    current_degree: usize,
    history: VecDeque<ParallelismStats>,
    max_history: usize,
}

impl AdaptiveParallelism {
    pub fn new(min_degree: usize, max_degree: usize) -> Self {
        Self {
            min_degree: min_degree.max(1),
            max_degree: max_degree.max(1),
            current_degree: min_degree.max(1),
            history: VecDeque::new(),
            max_history: 20,
        }
    }

    /// Report current system stats.
    pub fn report_stats(&mut self, stats: ParallelismStats) {
        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(stats);
    }

    /// Adjust parallelism degree based on recent stats.
    pub fn adjust(&mut self) -> usize {
        if self.history.is_empty() {
            return self.current_degree;
        }

        let avg_cpu: f64 =
            self.history.iter().map(|s| s.cpu_utilization).sum::<f64>() / self.history.len() as f64;
        let avg_io_wait: f64 =
            self.history.iter().map(|s| s.io_wait_ratio).sum::<f64>() / self.history.len() as f64;

        let new_degree = if avg_cpu < 0.5 && avg_io_wait < 0.3 {
            // Under-utilized: increase parallelism
            (self.current_degree + 1).min(self.max_degree)
        } else if avg_cpu > 0.9 || avg_io_wait > 0.7 {
            // Over-loaded: decrease parallelism
            (self.current_degree.saturating_sub(1)).max(self.min_degree)
        } else {
            self.current_degree
        };

        self.current_degree = new_degree;
        new_degree
    }

    pub fn current_degree(&self) -> usize {
        self.current_degree
    }

    pub fn set_degree(&mut self, degree: usize) {
        self.current_degree = degree.clamp(self.min_degree, self.max_degree);
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_window_row_number() {
        let def = WindowDef::new("row_number");
        let mut sw = StreamingWindow::new(def);
        assert_eq!(sw.process_row(&[10, 20]), 1);
        assert_eq!(sw.process_row(&[30, 40]), 2);
        assert_eq!(sw.process_row(&[50, 60]), 3);
        assert_eq!(sw.total_rows(), 3);
    }

    #[test]
    fn streaming_window_sum_partitioned() {
        let def = WindowDef::new("sum").with_partition(vec![0]).with_order(1);
        let mut sw = StreamingWindow::new(def);
        // partition=1, val=10
        assert_eq!(sw.process_row(&[1, 10]), 10);
        // partition=1, val=20
        assert_eq!(sw.process_row(&[1, 20]), 30);
        // partition=2, val=5
        assert_eq!(sw.process_row(&[2, 5]), 5);
    }

    #[test]
    fn sort_spill_threshold() {
        let mut mgr = SortSpillManager::new(SpillPolicy::ThresholdBased, 1000);
        assert!(!mgr.add_run(100, 500)); // 500 < 1000 => no spill
        assert!(mgr.add_run(100, 600)); // 1100 > 1000 => spill
        assert_eq!(mgr.spill_count(), 1);
        assert_eq!(mgr.run_count(), 2);
    }

    #[test]
    fn sort_spill_never() {
        let mut mgr = SortSpillManager::new(SpillPolicy::Never, 100);
        assert!(!mgr.add_run(1000, 9999)); // never spills
        assert_eq!(mgr.spill_count(), 0);
    }

    #[test]
    fn sort_spill_adaptive() {
        let mut mgr = SortSpillManager::new(SpillPolicy::Adaptive, 1000);
        assert!(!mgr.add_run(100, 700)); // 70% < 80% => no spill
        assert!(mgr.add_run(100, 200)); // 90% > 80% => spill
        assert_eq!(mgr.spill_count(), 1);
    }

    #[test]
    fn semi_join_execution() {
        let left = vec![1, 2, 3, 4, 5];
        let right = vec![2, 4, 6];
        let result = SemiAntiJoinOptimizer::execute_semi(&left, &right);
        assert_eq!(result, vec![1, 3]); // indices of 2 and 4
    }

    #[test]
    fn anti_join_execution() {
        let left = vec![1, 2, 3, 4, 5];
        let right = vec![2, 4, 6];
        let result = SemiAntiJoinOptimizer::execute_anti(&left, &right);
        assert_eq!(result, vec![0, 2, 4]); // indices of 1, 3, 5
    }

    #[test]
    fn semi_join_rewrite() {
        let mut opt = SemiAntiJoinOptimizer::new();
        let c = opt.rewrite_exists_to_semi("orders", "customers", "cust_id", 100);
        assert_eq!(c.rewritten_kind, JoinKind::Semi);
        assert_eq!(opt.rewrite_count(), 1);
    }

    #[test]
    fn adaptive_parallelism_scale_up() {
        let mut ap = AdaptiveParallelism::new(1, 8);
        ap.report_stats(ParallelismStats {
            cpu_utilization: 0.3,
            io_wait_ratio: 0.1,
            queue_depth: 2,
            active_queries: 1,
        });
        let new_deg = ap.adjust();
        assert!(new_deg > 1); // should scale up
    }

    #[test]
    fn adaptive_parallelism_scale_down() {
        let mut ap = AdaptiveParallelism::new(1, 8);
        ap.set_degree(6);
        ap.report_stats(ParallelismStats {
            cpu_utilization: 0.95,
            io_wait_ratio: 0.1,
            queue_depth: 10,
            active_queries: 6,
        });
        let new_deg = ap.adjust();
        assert!(new_deg < 6);
    }
}
