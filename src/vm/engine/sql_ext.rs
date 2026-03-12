// ── src/vm/engine/sql_ext.rs ──
// R20: SQL 功能扩展 — 窗口函数增强 / MERGE语句 / 物化视图自动刷新 / 批量UPSERT

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
// 1. WindowFuncEnhanced — 增强窗口函数
// ═══════════════════════════════════════════════════════════════════════

/// 窗口帧类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Rows,
    Range,
    Groups,
}

/// 窗口帧边界
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameBound {
    UnboundedPreceding,
    Preceding(usize),
    CurrentRow,
    Following(usize),
    UnboundedFollowing,
}

/// 增强窗口函数类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFuncType {
    RowNumber,
    Rank,
    DenseRank,
    Ntile(usize),
    Lead(usize),
    Lag(usize),
    FirstValue,
    LastValue,
    NthValue(usize),
    CumeDist,
    PercentRank,
}

/// 窗口函数定义
#[derive(Debug, Clone)]
pub struct WindowDef {
    pub func: WindowFuncType,
    pub partition_by: Vec<String>,
    pub order_by: Vec<(String, bool)>, // (column, ascending)
    pub frame_kind: FrameKind,
    pub frame_start: FrameBound,
    pub frame_end: FrameBound,
}

impl WindowDef {
    pub fn new(func: WindowFuncType) -> Self {
        Self {
            func,
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame_kind: FrameKind::Rows,
            frame_start: FrameBound::UnboundedPreceding,
            frame_end: FrameBound::CurrentRow,
        }
    }

    pub fn with_partition(mut self, cols: Vec<String>) -> Self {
        self.partition_by = cols;
        self
    }

    pub fn with_order(mut self, cols: Vec<(String, bool)>) -> Self {
        self.order_by = cols;
        self
    }

    pub fn with_frame(mut self, kind: FrameKind, start: FrameBound, end: FrameBound) -> Self {
        self.frame_kind = kind;
        self.frame_start = start;
        self.frame_end = end;
        self
    }
}

/// 窗口函数评估器
pub struct WindowFuncEvaluator {
    definitions: Vec<WindowDef>,
}

impl WindowFuncEvaluator {
    pub fn new() -> Self {
        Self {
            definitions: Vec::new(),
        }
    }

    pub fn add(&mut self, def: WindowDef) {
        self.definitions.push(def);
    }

    /// 对分区内行计算 row_number/rank 等
    pub fn eval_row_number(partition: &[Vec<i64>]) -> Vec<usize> {
        (1..=partition.len()).collect()
    }

    pub fn eval_rank(partition: &[Vec<i64>], order_col: usize) -> Vec<usize> {
        let mut ranks = vec![1usize; partition.len()];
        for i in 1..partition.len() {
            if partition[i][order_col] == partition[i - 1][order_col] {
                ranks[i] = ranks[i - 1];
            } else {
                ranks[i] = i + 1;
            }
        }
        ranks
    }

    pub fn eval_dense_rank(partition: &[Vec<i64>], order_col: usize) -> Vec<usize> {
        let mut ranks = vec![1usize; partition.len()];
        let mut cur_rank = 1;
        for i in 1..partition.len() {
            if partition[i][order_col] != partition[i - 1][order_col] {
                cur_rank += 1;
            }
            ranks[i] = cur_rank;
        }
        ranks
    }

    pub fn eval_ntile(n: usize, total_rows: usize) -> Vec<usize> {
        let base = total_rows / n;
        let remainder = total_rows % n;
        let mut result = Vec::with_capacity(total_rows);
        let mut tile = 1;
        let mut count = 0;
        let tile_size = |t: usize| -> usize {
            if t <= remainder {
                base + 1
            } else {
                base
            }
        };
        for _ in 0..total_rows {
            result.push(tile);
            count += 1;
            if count >= tile_size(tile) && tile < n {
                tile += 1;
                count = 0;
            }
        }
        result
    }

    pub fn eval_lead(values: &[i64], offset: usize, default: i64) -> Vec<i64> {
        values
            .iter()
            .enumerate()
            .map(|(i, _)| {
                if i + offset < values.len() {
                    values[i + offset]
                } else {
                    default
                }
            })
            .collect()
    }

    pub fn eval_lag(values: &[i64], offset: usize, default: i64) -> Vec<i64> {
        values
            .iter()
            .enumerate()
            .map(|(i, _)| {
                if i >= offset {
                    values[i - offset]
                } else {
                    default
                }
            })
            .collect()
    }

    pub fn definition_count(&self) -> usize {
        self.definitions.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. MergeStatement — MERGE (UPSERT ON CONFLICT) 语句
// ═══════════════════════════════════════════════════════════════════════

/// MERGE 匹配动作
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeAction {
    UpdateSet(Vec<(String, String)>), // (column, expr_str)
    InsertValues(Vec<String>),
    Delete,
    DoNothing,
}

/// MERGE 子句
#[derive(Debug, Clone)]
pub struct MergeClause {
    pub when_matched: bool,
    pub condition: Option<String>,
    pub action: MergeAction,
}

/// MERGE 语句
pub struct MergeStatement {
    pub target_table: String,
    pub source_table: String,
    pub join_condition: String,
    pub clauses: Vec<MergeClause>,
}

impl MergeStatement {
    pub fn new(target: &str, source: &str, join_cond: &str) -> Self {
        Self {
            target_table: target.to_string(),
            source_table: source.to_string(),
            join_condition: join_cond.to_string(),
            clauses: Vec::new(),
        }
    }

    pub fn when_matched(mut self, action: MergeAction) -> Self {
        self.clauses.push(MergeClause {
            when_matched: true,
            condition: None,
            action,
        });
        self
    }

    pub fn when_matched_and(mut self, cond: &str, action: MergeAction) -> Self {
        self.clauses.push(MergeClause {
            when_matched: true,
            condition: Some(cond.to_string()),
            action,
        });
        self
    }

    pub fn when_not_matched(mut self, action: MergeAction) -> Self {
        self.clauses.push(MergeClause {
            when_matched: false,
            condition: None,
            action,
        });
        self
    }

    pub fn clause_count(&self) -> usize {
        self.clauses.len()
    }

    /// 模拟执行统计
    pub fn simulate_execute(&self, matched_rows: usize, unmatched_rows: usize) -> MergeStats {
        let mut stats = MergeStats::default();
        for clause in &self.clauses {
            if clause.when_matched {
                match &clause.action {
                    MergeAction::UpdateSet(_) => stats.updated += matched_rows,
                    MergeAction::Delete => stats.deleted += matched_rows,
                    MergeAction::DoNothing => {}
                    MergeAction::InsertValues(_) => {}
                }
            } else {
                match &clause.action {
                    MergeAction::InsertValues(_) => stats.inserted += unmatched_rows,
                    MergeAction::DoNothing => {}
                    _ => {}
                }
            }
        }
        stats
    }
}

#[derive(Debug, Default)]
pub struct MergeStats {
    pub inserted: usize,
    pub updated: usize,
    pub deleted: usize,
}

impl MergeStats {
    pub fn total_affected(&self) -> usize {
        self.inserted + self.updated + self.deleted
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. MaterializedViewRefresh — 物化视图自动刷新
// ═══════════════════════════════════════════════════════════════════════

/// 刷新策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicy {
    OnCommit,         // 每次 COMMIT 后刷新
    Periodic(u64),    // 周期（秒）
    OnDemand,         // 手动刷新
    Threshold(usize), // 变更行数阈值
}

/// 物化视图追踪
#[derive(Debug, Clone)]
pub struct MaterializedViewTracker {
    pub view_name: String,
    pub source_tables: Vec<String>,
    pub query: String,
    pub policy: RefreshPolicy,
    pub last_refresh_ms: u64,
    pub refresh_count: u64,
    pub pending_changes: usize,
    pub stale: bool,
}

impl MaterializedViewTracker {
    pub fn new(name: &str, query: &str, sources: Vec<String>, policy: RefreshPolicy) -> Self {
        Self {
            view_name: name.to_string(),
            source_tables: sources,
            query: query.to_string(),
            policy,
            last_refresh_ms: 0,
            refresh_count: 0,
            pending_changes: 0,
            stale: true,
        }
    }

    pub fn notify_change(&mut self, table: &str, count: usize) {
        if self.source_tables.iter().any(|t| t == table) {
            self.pending_changes += count;
            self.stale = true;
        }
    }

    pub fn should_refresh(&self, current_ms: u64) -> bool {
        if !self.stale {
            return false;
        }
        match self.policy {
            RefreshPolicy::OnCommit => self.stale,
            RefreshPolicy::Periodic(interval) => {
                current_ms - self.last_refresh_ms >= interval * 1000
            }
            RefreshPolicy::OnDemand => false,
            RefreshPolicy::Threshold(t) => self.pending_changes >= t,
        }
    }

    pub fn mark_refreshed(&mut self, timestamp_ms: u64) {
        self.stale = false;
        self.pending_changes = 0;
        self.last_refresh_ms = timestamp_ms;
        self.refresh_count += 1;
    }
}

/// 物化视图管理器
pub struct MaterializedViewManager {
    views: HashMap<String, MaterializedViewTracker>,
}

impl MaterializedViewManager {
    pub fn new() -> Self {
        Self {
            views: HashMap::new(),
        }
    }

    pub fn register(&mut self, tracker: MaterializedViewTracker) {
        self.views.insert(tracker.view_name.clone(), tracker);
    }

    pub fn notify_table_change(&mut self, table: &str, count: usize) {
        for v in self.views.values_mut() {
            v.notify_change(table, count);
        }
    }

    pub fn views_needing_refresh(&self, current_ms: u64) -> Vec<&str> {
        self.views
            .values()
            .filter(|v| v.should_refresh(current_ms))
            .map(|v| v.view_name.as_str())
            .collect()
    }

    pub fn mark_refreshed(&mut self, name: &str, timestamp_ms: u64) -> bool {
        if let Some(v) = self.views.get_mut(name) {
            v.mark_refreshed(timestamp_ms);
            true
        } else {
            false
        }
    }

    pub fn view_count(&self) -> usize {
        self.views.len()
    }

    pub fn stale_count(&self) -> usize {
        self.views.values().filter(|v| v.stale).count()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. BatchUpsert — 批量 UPSERT
// ═══════════════════════════════════════════════════════════════════════

/// 冲突策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStrategy {
    Ignore,
    Replace,
    Update,
    Error,
}

/// 批量 UPSERT 执行器
pub struct BatchUpsert {
    table: String,
    conflict_column: String,
    strategy: ConflictStrategy,
    update_columns: Vec<String>,
    batch: Vec<Vec<String>>,
    inserted: usize,
    updated: usize,
    skipped: usize,
}

impl BatchUpsert {
    pub fn new(table: &str, conflict_col: &str, strategy: ConflictStrategy) -> Self {
        Self {
            table: table.to_string(),
            conflict_column: conflict_col.to_string(),
            strategy,
            update_columns: Vec::new(),
            batch: Vec::new(),
            inserted: 0,
            updated: 0,
            skipped: 0,
        }
    }

    pub fn with_update_columns(mut self, cols: Vec<String>) -> Self {
        self.update_columns = cols;
        self
    }

    pub fn add_row(&mut self, values: Vec<String>) {
        self.batch.push(values);
    }

    pub fn add_rows(&mut self, rows: Vec<Vec<String>>) {
        self.batch.extend(rows);
    }

    pub fn batch_size(&self) -> usize {
        self.batch.len()
    }

    /// 模拟执行：检查冲突并统计
    pub fn simulate(&mut self, existing_keys: &[String]) -> BatchUpsertResult {
        self.inserted = 0;
        self.updated = 0;
        self.skipped = 0;

        for row in &self.batch {
            let key = row.first().map(|s| s.as_str()).unwrap_or("");
            let conflicts = existing_keys.iter().any(|k| k == key);

            if conflicts {
                match self.strategy {
                    ConflictStrategy::Ignore => self.skipped += 1,
                    ConflictStrategy::Replace | ConflictStrategy::Update => self.updated += 1,
                    ConflictStrategy::Error => {
                        return BatchUpsertResult {
                            inserted: self.inserted,
                            updated: self.updated,
                            skipped: self.skipped,
                            errors: 1,
                        };
                    }
                }
            } else {
                self.inserted += 1;
            }
        }

        BatchUpsertResult {
            inserted: self.inserted,
            updated: self.updated,
            skipped: self.skipped,
            errors: 0,
        }
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn conflict_column(&self) -> &str {
        &self.conflict_column
    }

    pub fn strategy(&self) -> ConflictStrategy {
        self.strategy
    }
}

#[derive(Debug)]
pub struct BatchUpsertResult {
    pub inserted: usize,
    pub updated: usize,
    pub skipped: usize,
    pub errors: usize,
}

impl BatchUpsertResult {
    pub fn total_affected(&self) -> usize {
        self.inserted + self.updated
    }

    pub fn success(&self) -> bool {
        self.errors == 0
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_row_number() {
        let partition = vec![vec![10], vec![20], vec![30]];
        let rn = WindowFuncEvaluator::eval_row_number(&partition);
        assert_eq!(rn, vec![1, 2, 3]);
    }

    #[test]
    fn test_window_rank() {
        let partition = vec![vec![10], vec![10], vec![20], vec![30]];
        let ranks = WindowFuncEvaluator::eval_rank(&partition, 0);
        assert_eq!(ranks, vec![1, 1, 3, 4]);
    }

    #[test]
    fn test_window_dense_rank() {
        let partition = vec![vec![10], vec![10], vec![20], vec![30]];
        let ranks = WindowFuncEvaluator::eval_dense_rank(&partition, 0);
        assert_eq!(ranks, vec![1, 1, 2, 3]);
    }

    #[test]
    fn test_window_ntile() {
        let tiles = WindowFuncEvaluator::eval_ntile(3, 7);
        // 7 / 3 = 2 base, 1 remainder → tile 1 gets 3, tiles 2-3 get 2
        assert_eq!(tiles, vec![1, 1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn test_window_lead_lag() {
        let values = vec![10, 20, 30, 40, 50];
        let lead = WindowFuncEvaluator::eval_lead(&values, 2, -1);
        assert_eq!(lead, vec![30, 40, 50, -1, -1]);
        let lag = WindowFuncEvaluator::eval_lag(&values, 1, 0);
        assert_eq!(lag, vec![0, 10, 20, 30, 40]);
    }

    #[test]
    fn test_window_def_builder() {
        let def = WindowDef::new(WindowFuncType::RowNumber)
            .with_partition(vec!["dept".into()])
            .with_order(vec![("salary".into(), false)])
            .with_frame(
                FrameKind::Rows,
                FrameBound::Preceding(2),
                FrameBound::CurrentRow,
            );
        assert_eq!(def.func, WindowFuncType::RowNumber);
        assert_eq!(def.partition_by.len(), 1);
        assert_eq!(def.frame_kind, FrameKind::Rows);
    }

    #[test]
    fn test_merge_statement() {
        let merge = MergeStatement::new("target", "source", "target.id = source.id")
            .when_matched(MergeAction::UpdateSet(vec![(
                "name".into(),
                "source.name".into(),
            )]))
            .when_not_matched(MergeAction::InsertValues(vec![
                "source.id".into(),
                "source.name".into(),
            ]));
        assert_eq!(merge.clause_count(), 2);

        let stats = merge.simulate_execute(10, 5);
        assert_eq!(stats.updated, 10);
        assert_eq!(stats.inserted, 5);
        assert_eq!(stats.total_affected(), 15);
    }

    #[test]
    fn test_merge_delete_action() {
        let merge = MergeStatement::new("t", "s", "t.id = s.id").when_matched(MergeAction::Delete);
        let stats = merge.simulate_execute(3, 0);
        assert_eq!(stats.deleted, 3);
    }

    #[test]
    fn test_materialized_view_refresh() {
        let mut mgr = MaterializedViewManager::new();
        let tracker = MaterializedViewTracker::new(
            "mv_sales",
            "SELECT SUM(amount) FROM sales",
            vec!["sales".into()],
            RefreshPolicy::Threshold(100),
        );
        mgr.register(tracker);

        mgr.notify_table_change("sales", 50);
        assert!(mgr.views_needing_refresh(0).is_empty()); // 50 < 100
        mgr.notify_table_change("sales", 60);
        let needing = mgr.views_needing_refresh(0);
        assert_eq!(needing.len(), 1);
        assert_eq!(needing[0], "mv_sales");

        mgr.mark_refreshed("mv_sales", 1000);
        assert_eq!(mgr.stale_count(), 0);
    }

    #[test]
    fn test_materialized_view_periodic() {
        let mut v = MaterializedViewTracker::new(
            "mv_daily",
            "SELECT * FROM orders",
            vec!["orders".into()],
            RefreshPolicy::Periodic(60),
        );
        v.notify_change("orders", 1);
        assert!(!v.should_refresh(30_000)); // 30s < 60s
        assert!(v.should_refresh(61_000)); // 61s >= 60s
    }

    #[test]
    fn test_batch_upsert_ignore() {
        let mut upsert = BatchUpsert::new("users", "email", ConflictStrategy::Ignore);
        upsert.add_row(vec!["a@b.com".into(), "Alice".into()]);
        upsert.add_row(vec!["c@d.com".into(), "Carol".into()]);
        upsert.add_row(vec!["e@f.com".into(), "Eve".into()]);

        let result = upsert.simulate(&["a@b.com".to_string()]);
        assert_eq!(result.inserted, 2);
        assert_eq!(result.skipped, 1);
        assert!(result.success());
    }

    #[test]
    fn test_batch_upsert_update() {
        let mut upsert = BatchUpsert::new("products", "sku", ConflictStrategy::Update)
            .with_update_columns(vec!["price".into()]);
        upsert.add_rows(vec![
            vec!["SKU1".into(), "100".into()],
            vec!["SKU2".into(), "200".into()],
        ]);
        let result = upsert.simulate(&["SKU1".to_string()]);
        assert_eq!(result.inserted, 1);
        assert_eq!(result.updated, 1);
        assert_eq!(result.total_affected(), 2);
    }

    #[test]
    fn test_batch_upsert_error_strategy() {
        let mut upsert = BatchUpsert::new("t", "id", ConflictStrategy::Error);
        upsert.add_row(vec!["1".into()]);
        let result = upsert.simulate(&["1".to_string()]);
        assert_eq!(result.errors, 1);
        assert!(!result.success());
    }
}
