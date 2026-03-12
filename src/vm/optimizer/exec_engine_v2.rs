// ── src/vm/optimizer/exec_engine_v2.rs ──
// R21: 查询执行引擎深化 — 向量化执行2.0 / 表达式JIT / 并行查询 / 自适应内存管理

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
// 1. VectorizedEngine2 — 向量化执行引擎 2.0
// ═══════════════════════════════════════════════════════════════════════

/// 列数据类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColType {
    Int64,
    Float64,
    Text,
    Bool,
    Null,
}

/// 向量化列数据块
#[derive(Debug, Clone)]
pub enum ColumnVector {
    Ints(Vec<i64>),
    Floats(Vec<f64>),
    Texts(Vec<String>),
    Bools(Vec<bool>),
    Nulls(Vec<bool>), // null bitmap
}

impl ColumnVector {
    pub fn len(&self) -> usize {
        match self {
            Self::Ints(v) => v.len(),
            Self::Floats(v) => v.len(),
            Self::Texts(v) => v.len(),
            Self::Bools(v) => v.len(),
            Self::Nulls(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 执行批次
#[derive(Debug, Clone)]
pub struct DataBatch {
    pub columns: Vec<(String, ColumnVector)>,
    pub row_count: usize,
}

impl DataBatch {
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            row_count: 0,
        }
    }

    pub fn add_int_column(&mut self, name: &str, data: Vec<i64>) {
        self.row_count = data.len();
        self.columns
            .push((name.to_string(), ColumnVector::Ints(data)));
    }

    pub fn add_float_column(&mut self, name: &str, data: Vec<f64>) {
        self.row_count = data.len();
        self.columns
            .push((name.to_string(), ColumnVector::Floats(data)));
    }

    pub fn add_text_column(&mut self, name: &str, data: Vec<String>) {
        self.row_count = data.len();
        self.columns
            .push((name.to_string(), ColumnVector::Texts(data)));
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn get_column(&self, name: &str) -> Option<&ColumnVector> {
        self.columns.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

/// 向量化操作
#[derive(Debug, Clone)]
pub enum VecOp2 {
    FilterGt(String, i64),    // column > value
    FilterEq(String, i64),    // column == value
    Project(Vec<String>),     // select columns
    SumAgg(String),           // SUM(column)
    CountAgg,                 // COUNT(*)
    HashJoin(String, String), // join on columns
}

/// 向量化执行引擎 2.0
pub struct VectorizedEngine2 {
    batch_size: usize,
    ops_executed: u64,
    rows_processed: u64,
}

impl VectorizedEngine2 {
    pub fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            ops_executed: 0,
            rows_processed: 0,
        }
    }

    /// 过滤操作：column > threshold
    pub fn filter_gt(&mut self, batch: &DataBatch, col: &str, threshold: i64) -> DataBatch {
        self.ops_executed += 1;
        let mut result = DataBatch::new();
        let filter_col = match batch.get_column(col) {
            Some(ColumnVector::Ints(v)) => v,
            _ => return result,
        };
        let mask: Vec<bool> = filter_col.iter().map(|&v| v > threshold).collect();
        self.rows_processed += batch.row_count as u64;

        for (name, vec) in &batch.columns {
            match vec {
                ColumnVector::Ints(v) => {
                    let filtered: Vec<i64> = v
                        .iter()
                        .zip(&mask)
                        .filter(|(_, &m)| m)
                        .map(|(&v, _)| v)
                        .collect();
                    result.add_int_column(name, filtered);
                }
                ColumnVector::Texts(v) => {
                    let filtered: Vec<String> = v
                        .iter()
                        .zip(&mask)
                        .filter(|(_, &m)| m)
                        .map(|(v, _)| v.clone())
                        .collect();
                    result.add_text_column(name, filtered);
                }
                _ => {}
            }
        }
        result
    }

    /// 聚合操作：SUM(column)
    pub fn sum_int(&mut self, batch: &DataBatch, col: &str) -> i64 {
        self.ops_executed += 1;
        match batch.get_column(col) {
            Some(ColumnVector::Ints(v)) => {
                self.rows_processed += v.len() as u64;
                v.iter().sum()
            }
            _ => 0,
        }
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn ops_executed(&self) -> u64 {
        self.ops_executed
    }

    pub fn rows_processed(&self) -> u64 {
        self.rows_processed
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. ExprJit — 表达式即时编译
// ═══════════════════════════════════════════════════════════════════════

/// JIT 编译的表达式节点
#[derive(Debug, Clone)]
pub enum JitExpr {
    Const(i64),
    Column(usize),
    Add(Box<JitExpr>, Box<JitExpr>),
    Mul(Box<JitExpr>, Box<JitExpr>),
    Gt(Box<JitExpr>, Box<JitExpr>),
    And(Box<JitExpr>, Box<JitExpr>),
    Or(Box<JitExpr>, Box<JitExpr>),
    Neg(Box<JitExpr>),
}

/// JIT 编译结果
pub struct CompiledExpr {
    expr: JitExpr,
    eval_count: u64,
}

impl CompiledExpr {
    pub fn compile(expr: JitExpr) -> Self {
        Self {
            expr,
            eval_count: 0,
        }
    }

    /// 对单行求值
    pub fn eval(&mut self, row: &[i64]) -> i64 {
        self.eval_count += 1;
        Self::eval_node(&self.expr, row)
    }

    fn eval_node(node: &JitExpr, row: &[i64]) -> i64 {
        match node {
            JitExpr::Const(v) => *v,
            JitExpr::Column(idx) => row.get(*idx).copied().unwrap_or(0),
            JitExpr::Add(l, r) => Self::eval_node(l, row) + Self::eval_node(r, row),
            JitExpr::Mul(l, r) => Self::eval_node(l, row) * Self::eval_node(r, row),
            JitExpr::Gt(l, r) => {
                if Self::eval_node(l, row) > Self::eval_node(r, row) {
                    1
                } else {
                    0
                }
            }
            JitExpr::And(l, r) => {
                if Self::eval_node(l, row) != 0 && Self::eval_node(r, row) != 0 {
                    1
                } else {
                    0
                }
            }
            JitExpr::Or(l, r) => {
                if Self::eval_node(l, row) != 0 || Self::eval_node(r, row) != 0 {
                    1
                } else {
                    0
                }
            }
            JitExpr::Neg(e) => -Self::eval_node(e, row),
        }
    }

    /// 批量求值
    pub fn eval_batch(&mut self, rows: &[Vec<i64>]) -> Vec<i64> {
        rows.iter().map(|row| self.eval(row)).collect()
    }

    pub fn eval_count(&self) -> u64 {
        self.eval_count
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. ParallelQueryCoord — 并行查询协调器
// ═══════════════════════════════════════════════════════════════════════

/// 分区策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionStrategy {
    RoundRobin,
    Range,
    Hash,
}

/// 查询分片
#[derive(Debug, Clone)]
pub struct QueryShard {
    pub shard_id: usize,
    pub start_row: usize,
    pub end_row: usize,
    pub status: ShardStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardStatus {
    Pending,
    Running,
    Complete,
    Failed,
}

/// 并行查询协调器
pub struct ParallelQueryCoord {
    parallelism: usize,
    #[allow(dead_code)]
    strategy: PartitionStrategy,
    shards: Vec<QueryShard>,
    queries_planned: u64,
}

impl ParallelQueryCoord {
    pub fn new(parallelism: usize, strategy: PartitionStrategy) -> Self {
        Self {
            parallelism,
            strategy,
            shards: Vec::new(),
            queries_planned: 0,
        }
    }

    /// 将查询分为多个分片
    pub fn plan_shards(&mut self, total_rows: usize) -> Vec<QueryShard> {
        self.queries_planned += 1;
        let shard_size = (total_rows + self.parallelism - 1) / self.parallelism;
        self.shards = (0..self.parallelism)
            .map(|i| {
                let start = i * shard_size;
                let end = ((i + 1) * shard_size).min(total_rows);
                QueryShard {
                    shard_id: i,
                    start_row: start,
                    end_row: end,
                    status: ShardStatus::Pending,
                }
            })
            .filter(|s| s.start_row < s.end_row)
            .collect();
        self.shards.clone()
    }

    pub fn complete_shard(&mut self, shard_id: usize) {
        if let Some(s) = self.shards.iter_mut().find(|s| s.shard_id == shard_id) {
            s.status = ShardStatus::Complete;
        }
    }

    pub fn all_complete(&self) -> bool {
        !self.shards.is_empty()
            && self
                .shards
                .iter()
                .all(|s| s.status == ShardStatus::Complete)
    }

    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .shards
            .iter()
            .filter(|s| s.status == ShardStatus::Complete)
            .count();
        (done, self.shards.len())
    }

    pub fn parallelism(&self) -> usize {
        self.parallelism
    }

    pub fn queries_planned(&self) -> u64 {
        self.queries_planned
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. AdaptiveMemoryManager — 自适应内存管理
// ═══════════════════════════════════════════════════════════════════════

/// 内存区域
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemRegion {
    BufferPool,
    SortBuffer,
    HashTable,
    ResultSet,
    Temp,
}

/// 内存分配记录
#[derive(Debug, Clone)]
pub struct MemAllocation {
    pub region: MemRegion,
    pub allocated_bytes: usize,
    pub peak_bytes: usize,
    pub alloc_count: u64,
}

/// 自适应内存管理器
pub struct AdaptiveMemoryManager {
    total_budget: usize,
    allocations: HashMap<MemRegion, MemAllocation>,
    spill_threshold: f64,
    spill_count: u64,
}

impl AdaptiveMemoryManager {
    pub fn new(total_budget: usize, spill_threshold: f64) -> Self {
        Self {
            total_budget,
            allocations: HashMap::new(),
            spill_threshold,
            spill_count: 0,
        }
    }

    pub fn allocate(&mut self, region: MemRegion, bytes: usize) -> bool {
        let current_total: usize = self.allocations.values().map(|a| a.allocated_bytes).sum();
        if current_total + bytes > self.total_budget {
            return false;
        }
        let alloc = self.allocations.entry(region).or_insert(MemAllocation {
            region,
            allocated_bytes: 0,
            peak_bytes: 0,
            alloc_count: 0,
        });
        alloc.allocated_bytes += bytes;
        alloc.alloc_count += 1;
        if alloc.allocated_bytes > alloc.peak_bytes {
            alloc.peak_bytes = alloc.allocated_bytes;
        }
        true
    }

    pub fn release(&mut self, region: MemRegion, bytes: usize) {
        if let Some(alloc) = self.allocations.get_mut(&region) {
            alloc.allocated_bytes = alloc.allocated_bytes.saturating_sub(bytes);
        }
    }

    pub fn should_spill(&self) -> bool {
        let used: usize = self.allocations.values().map(|a| a.allocated_bytes).sum();
        (used as f64 / self.total_budget as f64) >= self.spill_threshold
    }

    pub fn trigger_spill(&mut self, region: MemRegion) -> usize {
        let freed = self
            .allocations
            .get(&region)
            .map(|a| a.allocated_bytes)
            .unwrap_or(0);
        if let Some(alloc) = self.allocations.get_mut(&region) {
            alloc.allocated_bytes = 0;
        }
        self.spill_count += 1;
        freed
    }

    pub fn used_bytes(&self) -> usize {
        self.allocations.values().map(|a| a.allocated_bytes).sum()
    }

    pub fn region_usage(&self, region: MemRegion) -> usize {
        self.allocations
            .get(&region)
            .map(|a| a.allocated_bytes)
            .unwrap_or(0)
    }

    pub fn peak_usage(&self, region: MemRegion) -> usize {
        self.allocations
            .get(&region)
            .map(|a| a.peak_bytes)
            .unwrap_or(0)
    }

    pub fn utilization(&self) -> f64 {
        self.used_bytes() as f64 / self.total_budget as f64
    }

    pub fn spill_count(&self) -> u64 {
        self.spill_count
    }

    pub fn total_budget(&self) -> usize {
        self.total_budget
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vectorized2_filter_gt() {
        let mut engine = VectorizedEngine2::new(1024);
        let mut batch = DataBatch::new();
        batch.add_int_column("id", vec![1, 2, 3, 4, 5]);
        batch.add_int_column("value", vec![10, 20, 30, 40, 50]);

        let result = engine.filter_gt(&batch, "value", 25);
        let ids = result.get_column("id").unwrap();
        match ids {
            ColumnVector::Ints(v) => assert_eq!(v, &[3, 4, 5]),
            _ => panic!("expected ints"),
        }
        assert_eq!(engine.ops_executed(), 1);
    }

    #[test]
    fn test_vectorized2_sum() {
        let mut engine = VectorizedEngine2::new(512);
        let mut batch = DataBatch::new();
        batch.add_int_column("amount", vec![100, 200, 300]);
        assert_eq!(engine.sum_int(&batch, "amount"), 600);
    }

    #[test]
    fn test_data_batch_columns() {
        let mut batch = DataBatch::new();
        batch.add_int_column("id", vec![1, 2]);
        batch.add_text_column("name", vec!["a".into(), "b".into()]);
        assert_eq!(batch.column_count(), 2);
        assert_eq!(batch.row_count, 2);
        assert!(batch.get_column("id").is_some());
        assert!(batch.get_column("missing").is_none());
    }

    #[test]
    fn test_jit_expr_arithmetic() {
        // (col0 + 10) * col1
        let expr = JitExpr::Mul(
            Box::new(JitExpr::Add(
                Box::new(JitExpr::Column(0)),
                Box::new(JitExpr::Const(10)),
            )),
            Box::new(JitExpr::Column(1)),
        );
        let mut compiled = CompiledExpr::compile(expr);
        assert_eq!(compiled.eval(&[5, 3]), 45); // (5+10)*3
        assert_eq!(compiled.eval(&[0, 7]), 70); // (0+10)*7
        assert_eq!(compiled.eval_count(), 2);
    }

    #[test]
    fn test_jit_expr_comparison() {
        // col0 > 10 AND col1 > 20
        let expr = JitExpr::And(
            Box::new(JitExpr::Gt(
                Box::new(JitExpr::Column(0)),
                Box::new(JitExpr::Const(10)),
            )),
            Box::new(JitExpr::Gt(
                Box::new(JitExpr::Column(1)),
                Box::new(JitExpr::Const(20)),
            )),
        );
        let mut compiled = CompiledExpr::compile(expr);
        assert_eq!(compiled.eval(&[15, 25]), 1);
        assert_eq!(compiled.eval(&[5, 25]), 0);
    }

    #[test]
    fn test_jit_batch_eval() {
        let expr = JitExpr::Add(Box::new(JitExpr::Column(0)), Box::new(JitExpr::Const(1)));
        let mut compiled = CompiledExpr::compile(expr);
        let results = compiled.eval_batch(&[vec![10], vec![20], vec![30]]);
        assert_eq!(results, vec![11, 21, 31]);
    }

    #[test]
    fn test_parallel_query_shards() {
        let mut coord = ParallelQueryCoord::new(4, PartitionStrategy::RoundRobin);
        let shards = coord.plan_shards(100);
        assert_eq!(shards.len(), 4);
        assert_eq!(shards[0].start_row, 0);
        assert_eq!(shards[0].end_row, 25);
        assert_eq!(shards[3].start_row, 75);
        assert_eq!(shards[3].end_row, 100);
    }

    #[test]
    fn test_parallel_query_progress() {
        let mut coord = ParallelQueryCoord::new(3, PartitionStrategy::Hash);
        coord.plan_shards(30);
        assert_eq!(coord.progress(), (0, 3));
        coord.complete_shard(0);
        coord.complete_shard(1);
        assert_eq!(coord.progress(), (2, 3));
        assert!(!coord.all_complete());
        coord.complete_shard(2);
        assert!(coord.all_complete());
    }

    #[test]
    fn test_adaptive_memory_allocate() {
        let mut mgr = AdaptiveMemoryManager::new(1024 * 1024, 0.8);
        assert!(mgr.allocate(MemRegion::BufferPool, 512 * 1024));
        assert!(mgr.allocate(MemRegion::SortBuffer, 256 * 1024));
        assert_eq!(mgr.used_bytes(), 768 * 1024);
        assert!(!mgr.should_spill()); // 75% < 80%
    }

    #[test]
    fn test_adaptive_memory_spill() {
        let mut mgr = AdaptiveMemoryManager::new(1000, 0.8);
        mgr.allocate(MemRegion::HashTable, 900);
        assert!(mgr.should_spill()); // 90% >= 80%
        let freed = mgr.trigger_spill(MemRegion::HashTable);
        assert_eq!(freed, 900);
        assert_eq!(mgr.spill_count(), 1);
        assert_eq!(mgr.used_bytes(), 0);
    }

    #[test]
    fn test_adaptive_memory_peak() {
        let mut mgr = AdaptiveMemoryManager::new(10000, 0.9);
        mgr.allocate(MemRegion::Temp, 500);
        mgr.allocate(MemRegion::Temp, 300);
        assert_eq!(mgr.peak_usage(MemRegion::Temp), 800);
        mgr.release(MemRegion::Temp, 600);
        assert_eq!(mgr.region_usage(MemRegion::Temp), 200);
        assert_eq!(mgr.peak_usage(MemRegion::Temp), 800); // peak unchanged
    }

    #[test]
    fn test_memory_budget_exceeded() {
        let mut mgr = AdaptiveMemoryManager::new(100, 0.5);
        assert!(mgr.allocate(MemRegion::ResultSet, 80));
        assert!(!mgr.allocate(MemRegion::ResultSet, 30)); // exceeds budget
    }
}
