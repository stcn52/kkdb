// ── src/vm/engine/sql_pipeline.rs ──
// R22: SQL 执行管线增强 — 流式查询处理 / 多阶段聚合 / 子查询优化 / 执行计划缓存池

use std::collections::{HashMap, VecDeque};

// ═══════════════════════════════════════════════════════════════════════
// 1. StreamProcessor — 流式查询处理
// ═══════════════════════════════════════════════════════════════════════

/// 流式数据块
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub chunk_id: u64,
    pub rows: Vec<Vec<i64>>,
    pub is_last: bool,
}

/// 流处理算子
#[derive(Debug, Clone)]
pub enum StreamOp {
    Filter { column_idx: usize, threshold: i64 },
    Project { column_indices: Vec<usize> },
    Limit { count: usize },
    Map { column_idx: usize, offset: i64 },
}

/// 流式查询处理器
pub struct StreamProcessor {
    pipeline: Vec<StreamOp>,
    chunks_processed: u64,
    rows_in: u64,
    rows_out: u64,
}

impl StreamProcessor {
    pub fn new() -> Self {
        Self {
            pipeline: Vec::new(),
            chunks_processed: 0,
            rows_in: 0,
            rows_out: 0,
        }
    }

    pub fn add_op(&mut self, op: StreamOp) {
        self.pipeline.push(op);
    }

    /// 处理一个数据块
    pub fn process(&mut self, chunk: StreamChunk) -> StreamChunk {
        self.chunks_processed += 1;
        self.rows_in += chunk.rows.len() as u64;
        let mut rows = chunk.rows;

        for op in &self.pipeline {
            rows = match op {
                StreamOp::Filter { column_idx, threshold } => {
                    rows.into_iter()
                        .filter(|row| row.get(*column_idx).map_or(false, |v| *v > *threshold))
                        .collect()
                }
                StreamOp::Project { column_indices } => {
                    rows.into_iter()
                        .map(|row| {
                            column_indices.iter()
                                .filter_map(|&idx| row.get(idx).copied())
                                .collect()
                        })
                        .collect()
                }
                StreamOp::Limit { count } => {
                    rows.into_iter().take(*count).collect()
                }
                StreamOp::Map { column_idx, offset } => {
                    rows.into_iter()
                        .map(|mut row| {
                            if let Some(v) = row.get_mut(*column_idx) {
                                *v += offset;
                            }
                            row
                        })
                        .collect()
                }
            };
        }

        self.rows_out += rows.len() as u64;
        StreamChunk {
            chunk_id: chunk.chunk_id,
            rows,
            is_last: chunk.is_last,
        }
    }

    pub fn selectivity(&self) -> f64 {
        if self.rows_in == 0 { return 1.0; }
        self.rows_out as f64 / self.rows_in as f64
    }

    pub fn chunks_processed(&self) -> u64 {
        self.chunks_processed
    }

    pub fn pipeline_depth(&self) -> usize {
        self.pipeline.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. MultiStageAggregator — 多阶段聚合
// ═══════════════════════════════════════════════════════════════════════

/// 聚合函数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    Sum,
    Count,
    Min,
    Max,
    Avg,
}

/// 聚合阶段
#[derive(Debug, Clone)]
pub struct AggStage {
    pub stage_id: usize,
    pub func: AggFunc,
    pub column_idx: usize,
    pub partial_results: HashMap<String, f64>,
    pub count_map: HashMap<String, u64>,
}

/// 多阶段聚合器
pub struct MultiStageAggregator {
    stages: Vec<AggStage>,
    group_key_idx: usize,
}

impl MultiStageAggregator {
    pub fn new(group_key_idx: usize) -> Self {
        Self {
            stages: Vec::new(),
            group_key_idx,
        }
    }

    pub fn add_stage(&mut self, func: AggFunc, column_idx: usize) {
        let id = self.stages.len();
        self.stages.push(AggStage {
            stage_id: id,
            func,
            column_idx,
            partial_results: HashMap::new(),
            count_map: HashMap::new(),
        });
    }

    /// 部分聚合（第一阶段）
    pub fn partial_aggregate(&mut self, rows: &[Vec<i64>]) {
        for row in rows {
            let group_key = row.get(self.group_key_idx)
                .map(|v| v.to_string())
                .unwrap_or_default();

            for stage in &mut self.stages {
                let val = row.get(stage.column_idx).copied().unwrap_or(0) as f64;
                let entry = stage.partial_results.entry(group_key.clone()).or_insert(0.0);
                let count = stage.count_map.entry(group_key.clone()).or_insert(0);

                match stage.func {
                    AggFunc::Sum | AggFunc::Avg => { *entry += val; }
                    AggFunc::Count => { *entry += 1.0; }
                    AggFunc::Min => {
                        if *count == 0 || val < *entry { *entry = val; }
                    }
                    AggFunc::Max => {
                        if *count == 0 || val > *entry { *entry = val; }
                    }
                }
                *count += 1;
            }
        }
    }

    /// 最终聚合（合并部分结果）
    pub fn finalize(&self) -> Vec<(String, Vec<f64>)> {
        let mut groups: HashMap<String, Vec<f64>> = HashMap::new();
        for stage in &self.stages {
            for (key, &val) in &stage.partial_results {
                let group = groups.entry(key.clone()).or_default();
                while group.len() <= stage.stage_id {
                    group.push(0.0);
                }
                let final_val = match stage.func {
                    AggFunc::Avg => {
                        let cnt = stage.count_map.get(key).copied().unwrap_or(1);
                        if cnt > 0 { val / cnt as f64 } else { 0.0 }
                    }
                    _ => val,
                };
                group[stage.stage_id] = final_val;
            }
        }
        let mut result: Vec<(String, Vec<f64>)> = groups.into_iter().collect();
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }

    pub fn group_count(&self) -> usize {
        if self.stages.is_empty() { return 0; }
        self.stages[0].partial_results.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. SubqueryOptimizer — 子查询优化
// ═══════════════════════════════════════════════════════════════════════

/// 子查询类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubqueryType {
    Scalar,          // 返回单值
    Exists,          // EXISTS 检查
    In,              // IN (subquery)
    Correlated,      // 相关子查询
    Lateral,         // LATERAL 子查询
}

/// 优化策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteStrategy {
    SemiJoin,         // EXISTS → semi-join
    AntiJoin,         // NOT EXISTS → anti-join
    Materialize,      // 缓存子查询结果
    Decorrelate,      // 解除相关性
    NoRewrite,        // 保持原样
}

/// 子查询优化器
pub struct SubqueryOptimizer {
    rewrites: Vec<(SubqueryType, RewriteStrategy)>,
    optimizations_applied: u64,
}

impl SubqueryOptimizer {
    pub fn new() -> Self {
        Self {
            rewrites: Vec::new(),
            optimizations_applied: 0,
        }
    }

    /// 根据子查询类型推荐优化策略
    pub fn recommend(&self, sq_type: SubqueryType, is_negated: bool) -> RewriteStrategy {
        match (sq_type, is_negated) {
            (SubqueryType::Exists, false) => RewriteStrategy::SemiJoin,
            (SubqueryType::Exists, true) => RewriteStrategy::AntiJoin,
            (SubqueryType::In, _) => RewriteStrategy::SemiJoin,
            (SubqueryType::Correlated, _) => RewriteStrategy::Decorrelate,
            (SubqueryType::Scalar, _) => RewriteStrategy::Materialize,
            (SubqueryType::Lateral, _) => RewriteStrategy::NoRewrite,
        }
    }

    pub fn apply_rewrite(&mut self, sq_type: SubqueryType, strategy: RewriteStrategy) {
        self.rewrites.push((sq_type, strategy));
        self.optimizations_applied += 1;
    }

    pub fn rewrite_count(&self) -> u64 {
        self.optimizations_applied
    }

    pub fn rewrites(&self) -> &[(SubqueryType, RewriteStrategy)] {
        &self.rewrites
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. PlanCachePool — 执行计划缓存池
// ═══════════════════════════════════════════════════════════════════════

/// 缓存的执行计划
#[derive(Debug, Clone)]
pub struct CachedPlan {
    pub plan_id: u64,
    pub sql_hash: u64,
    pub sql_text: String,
    pub cost_estimate: f64,
    pub hit_count: u64,
    pub created_ms: u64,
    pub last_used_ms: u64,
}

/// 执行计划缓存池
pub struct PlanCachePool {
    plans: VecDeque<CachedPlan>,
    max_size: usize,
    next_plan_id: u64,
    total_hits: u64,
    total_misses: u64,
}

impl PlanCachePool {
    pub fn new(max_size: usize) -> Self {
        Self {
            plans: VecDeque::new(),
            max_size,
            next_plan_id: 1,
            total_hits: 0,
            total_misses: 0,
        }
    }

    fn simple_hash(s: &str) -> u64 {
        let mut h: u64 = 5381;
        for b in s.bytes() {
            h = h.wrapping_mul(33).wrapping_add(b as u64);
        }
        h
    }

    pub fn lookup(&mut self, sql: &str) -> Option<&CachedPlan> {
        let hash = Self::simple_hash(sql);
        if let Some(plan) = self.plans.iter_mut().find(|p| p.sql_hash == hash) {
            plan.hit_count += 1;
            self.total_hits += 1;
            // borrow checker: return immutable
            let hash2 = hash;
            return self.plans.iter().find(|p| p.sql_hash == hash2);
        }
        self.total_misses += 1;
        None
    }

    pub fn insert(&mut self, sql: &str, cost_estimate: f64, timestamp_ms: u64) -> u64 {
        let hash = Self::simple_hash(sql);
        // Evict LRU if at capacity
        if self.plans.len() >= self.max_size {
            self.plans.pop_front();
        }
        let id = self.next_plan_id;
        self.next_plan_id += 1;
        self.plans.push_back(CachedPlan {
            plan_id: id,
            sql_hash: hash,
            sql_text: sql.to_string(),
            cost_estimate,
            hit_count: 0,
            created_ms: timestamp_ms,
            last_used_ms: timestamp_ms,
        });
        id
    }

    pub fn invalidate(&mut self, sql: &str) {
        let hash = Self::simple_hash(sql);
        self.plans.retain(|p| p.sql_hash != hash);
    }

    pub fn size(&self) -> usize {
        self.plans.len()
    }

    pub fn hit_rate(&self) -> f64 {
        let total = self.total_hits + self.total_misses;
        if total == 0 { return 0.0; }
        self.total_hits as f64 / total as f64
    }

    pub fn total_hits(&self) -> u64 {
        self.total_hits
    }

    pub fn clear(&mut self) {
        self.plans.clear();
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_processor_filter_project() {
        let mut sp = StreamProcessor::new();
        sp.add_op(StreamOp::Filter { column_idx: 0, threshold: 2 });
        sp.add_op(StreamOp::Project { column_indices: vec![1] });

        let chunk = StreamChunk {
            chunk_id: 1,
            rows: vec![
                vec![1, 10], vec![2, 20], vec![3, 30], vec![4, 40], vec![5, 50],
            ],
            is_last: true,
        };
        let result = sp.process(chunk);
        assert_eq!(result.rows.len(), 3); // 3,4,5 pass filter
        assert_eq!(result.rows[0], vec![30]); // projected col 1
        assert!(sp.selectivity() < 1.0);
    }

    #[test]
    fn test_stream_processor_limit() {
        let mut sp = StreamProcessor::new();
        sp.add_op(StreamOp::Limit { count: 2 });
        let chunk = StreamChunk {
            chunk_id: 1,
            rows: vec![vec![1], vec![2], vec![3], vec![4]],
            is_last: false,
        };
        let result = sp.process(chunk);
        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn test_multi_stage_aggregator() {
        let mut agg = MultiStageAggregator::new(0); // group by col 0
        agg.add_stage(AggFunc::Sum, 1);
        agg.add_stage(AggFunc::Count, 1);

        agg.partial_aggregate(&[
            vec![1, 10], vec![1, 20], vec![2, 30], vec![2, 40], vec![2, 50],
        ]);
        let results = agg.finalize();
        assert_eq!(results.len(), 2);
        let g1 = results.iter().find(|(k, _)| k == "1").unwrap();
        assert_eq!(g1.1[0], 30.0); // sum of 10+20
        assert_eq!(g1.1[1], 2.0); // count 2
    }

    #[test]
    fn test_multi_stage_avg() {
        let mut agg = MultiStageAggregator::new(0);
        agg.add_stage(AggFunc::Avg, 1);
        agg.partial_aggregate(&[vec![1, 10], vec![1, 20], vec![1, 30]]);
        let results = agg.finalize();
        let g1 = results.iter().find(|(k, _)| k == "1").unwrap();
        assert!((g1.1[0] - 20.0).abs() < 0.1); // avg of 10,20,30
    }

    #[test]
    fn test_subquery_optimizer_recommend() {
        let opt = SubqueryOptimizer::new();
        assert_eq!(opt.recommend(SubqueryType::Exists, false), RewriteStrategy::SemiJoin);
        assert_eq!(opt.recommend(SubqueryType::Exists, true), RewriteStrategy::AntiJoin);
        assert_eq!(opt.recommend(SubqueryType::Correlated, false), RewriteStrategy::Decorrelate);
        assert_eq!(opt.recommend(SubqueryType::Scalar, false), RewriteStrategy::Materialize);
    }

    #[test]
    fn test_subquery_optimizer_apply() {
        let mut opt = SubqueryOptimizer::new();
        opt.apply_rewrite(SubqueryType::In, RewriteStrategy::SemiJoin);
        opt.apply_rewrite(SubqueryType::Exists, RewriteStrategy::AntiJoin);
        assert_eq!(opt.rewrite_count(), 2);
        assert_eq!(opt.rewrites().len(), 2);
    }

    #[test]
    fn test_plan_cache_pool() {
        let mut cache = PlanCachePool::new(3);
        cache.insert("SELECT * FROM t", 10.0, 1000);
        cache.insert("INSERT INTO t VALUES (1)", 5.0, 1001);

        let hit = cache.lookup("SELECT * FROM t");
        assert!(hit.is_some());
        assert_eq!(cache.total_hits(), 1);

        let miss = cache.lookup("DELETE FROM t");
        assert!(miss.is_none());
        assert!(cache.hit_rate() > 0.0);
    }

    #[test]
    fn test_plan_cache_eviction() {
        let mut cache = PlanCachePool::new(2);
        cache.insert("q1", 1.0, 100);
        cache.insert("q2", 2.0, 200);
        cache.insert("q3", 3.0, 300); // evicts q1
        assert_eq!(cache.size(), 2);
        let miss = cache.lookup("q1");
        assert!(miss.is_none());
    }

    #[test]
    fn test_plan_cache_invalidate() {
        let mut cache = PlanCachePool::new(10);
        cache.insert("SELECT 1", 1.0, 100);
        assert_eq!(cache.size(), 1);
        cache.invalidate("SELECT 1");
        assert_eq!(cache.size(), 0);
    }

    #[test]
    fn test_stream_map_op() {
        let mut sp = StreamProcessor::new();
        sp.add_op(StreamOp::Map { column_idx: 0, offset: 100 });
        let chunk = StreamChunk {
            chunk_id: 1,
            rows: vec![vec![1, 2], vec![3, 4]],
            is_last: true,
        };
        let result = sp.process(chunk);
        assert_eq!(result.rows[0][0], 101);
        assert_eq!(result.rows[1][0], 103);
    }
}
