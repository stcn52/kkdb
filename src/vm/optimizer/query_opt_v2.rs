//! 查询优化器马力升级 — 全局索引优化 / 查询重写规则 / 自动索引推荐 / 统计信息增强
//!
//! Round 23 feature module providing advanced query optimization:
//! - `GlobalIndexOptimizer` — cross-table index selection and covering index optimization
//! - `QueryRewriter` — rule-based query rewriting engine
//! - `AutoIndexAdvisor` — automatic index recommendation based on workload analysis
//! - `StatsEnhancer` — enhanced table/column statistics collection

use std::collections::HashMap;

// ─── Global Index Optimizer ──────────────────────────────────────────

/// Describes an index available on a table.
#[derive(Debug, Clone)]
pub struct IndexDescriptor {
    pub index_name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    pub is_covering: bool,
    pub selectivity: f64, // 0.0 = fully selective, 1.0 = no selectivity
}

/// Recommendation from the index optimizer.
#[derive(Debug, Clone)]
pub struct IndexSelection {
    pub index_name: String,
    pub estimated_cost: f64,
    pub reason: String,
}

/// Optimizes index selection across tables and queries.
pub struct GlobalIndexOptimizer {
    indexes: HashMap<String, Vec<IndexDescriptor>>,
}

impl Default for GlobalIndexOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalIndexOptimizer {
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
        }
    }

    /// Register an index.
    pub fn register_index(&mut self, idx: IndexDescriptor) {
        self.indexes
            .entry(idx.table_name.clone())
            .or_default()
            .push(idx);
    }

    /// Find the best index for a table given the columns used in predicates.
    pub fn best_index(&self, table: &str, predicate_columns: &[&str]) -> Option<IndexSelection> {
        let table_indexes = self.indexes.get(table)?;
        let mut best: Option<(f64, &IndexDescriptor)> = None;

        for idx in table_indexes {
            // Score: how many predicate columns are covered by the index
            let covered = predicate_columns
                .iter()
                .filter(|c| idx.columns.contains(&c.to_string()))
                .count();
            if covered == 0 {
                continue;
            }
            let coverage_ratio = covered as f64 / predicate_columns.len() as f64;
            // Lower cost = better (combine coverage with selectivity)
            let cost = (1.0 - coverage_ratio) + idx.selectivity;
            if let Some((best_cost, _)) = &best {
                if cost < *best_cost {
                    best = Some((cost, idx));
                }
            } else {
                best = Some((cost, idx));
            }
        }

        best.map(|(cost, idx)| IndexSelection {
            index_name: idx.index_name.clone(),
            estimated_cost: cost,
            reason: format!(
                "Index {} covers predicates on {}",
                idx.index_name,
                idx.columns.join(", ")
            ),
        })
    }

    /// Find covering indexes that can satisfy a query without table access.
    pub fn find_covering_indexes(
        &self,
        table: &str,
        needed_columns: &[&str],
    ) -> Vec<&IndexDescriptor> {
        self.indexes
            .get(table)
            .map(|idxs| {
                idxs.iter()
                    .filter(|idx| {
                        needed_columns
                            .iter()
                            .all(|c| idx.columns.contains(&c.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all indexes for a table.
    pub fn indexes_for(&self, table: &str) -> Vec<&IndexDescriptor> {
        self.indexes
            .get(table)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    /// Total indexes known.
    pub fn total_indexes(&self) -> usize {
        self.indexes.values().map(|v| v.len()).sum()
    }
}

// ─── Query Rewriter ──────────────────────────────────────────────────

/// A rewrite rule that can transform a query pattern.
#[derive(Debug, Clone)]
pub struct RewriteRule {
    pub name: String,
    pub pattern: RewritePattern,
    pub priority: u32,
    pub enabled: bool,
}

/// Pattern types for query rewriting.
#[derive(Debug, Clone, PartialEq)]
pub enum RewritePattern {
    /// Fold constant expressions (e.g., 1+1 → 2)
    ConstantFolding,
    /// Remove redundant predicates (e.g., x > 5 AND x > 3 → x > 5)
    PredicateSimplification,
    /// Rewrite ANY/EXISTS subqueries to joins
    SubqueryToJoin,
    /// Push predicates down through joins
    PredicatePushdown,
    /// Eliminate unnecessary DISTINCT
    DistinctElimination,
    /// Merge adjacent projections
    ProjectionMerge,
}

/// Result of a rewrite application.
#[derive(Debug, Clone)]
pub struct RewriteResult {
    pub rule_name: String,
    pub applied: bool,
    pub description: String,
}

/// Rule-based query rewriting engine.
pub struct QueryRewriter {
    rules: Vec<RewriteRule>,
    stats: HashMap<String, u64>, // rule_name -> application count
}

impl Default for QueryRewriter {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryRewriter {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            stats: HashMap::new(),
        }
    }

    /// Add a rewrite rule.
    pub fn add_rule(&mut self, rule: RewriteRule) {
        self.rules.push(rule);
        // Keep sorted by priority (higher priority first)
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Enable or disable a rule by name.
    pub fn set_rule_enabled(&mut self, name: &str, enabled: bool) -> bool {
        for rule in &mut self.rules {
            if rule.name == name {
                rule.enabled = enabled;
                return true;
            }
        }
        false
    }

    /// Apply all matching rules to a query representation.
    /// In this simplified model, we check patterns against provided tags.
    pub fn apply_rules(&mut self, query_tags: &[RewritePattern]) -> Vec<RewriteResult> {
        let mut results = Vec::new();
        for rule in &self.rules {
            if !rule.enabled {
                continue;
            }
            let matches = query_tags.contains(&rule.pattern);
            if matches {
                *self.stats.entry(rule.name.clone()).or_insert(0) += 1;
                results.push(RewriteResult {
                    rule_name: rule.name.clone(),
                    applied: true,
                    description: format!("Applied rule '{}' ({:?})", rule.name, rule.pattern),
                });
            }
        }
        results
    }

    /// Get application count for a rule.
    pub fn rule_applications(&self, name: &str) -> u64 {
        self.stats.get(name).copied().unwrap_or(0)
    }

    /// Total number of rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Get enabled rules sorted by priority.
    pub fn enabled_rules(&self) -> Vec<&RewriteRule> {
        self.rules.iter().filter(|r| r.enabled).collect()
    }
}

// ─── Auto Index Advisor ──────────────────────────────────────────────

/// A column access pattern observed in workload.
#[derive(Debug, Clone)]
pub struct ColumnAccess {
    pub table: String,
    pub column: String,
    pub access_type: AccessType,
    pub frequency: u64,
}

/// Type of column access.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessType {
    EqualityFilter,
    RangeFilter,
    Join,
    OrderBy,
    GroupBy,
}

/// An index recommendation.
#[derive(Debug, Clone)]
pub struct IndexRecommendation {
    pub table: String,
    pub columns: Vec<String>,
    pub benefit_score: f64,
    pub reason: String,
}

/// Automatic index advisor based on workload analysis.
pub struct AutoIndexAdvisor {
    accesses: Vec<ColumnAccess>,
    existing_indexes: Vec<(String, Vec<String>)>, // (table, columns)
    max_recommendations: usize,
}

impl AutoIndexAdvisor {
    pub fn new(max_recommendations: usize) -> Self {
        Self {
            accesses: Vec::new(),
            existing_indexes: Vec::new(),
            max_recommendations,
        }
    }

    /// Record a column access pattern.
    pub fn record_access(&mut self, access: ColumnAccess) {
        self.accesses.push(access);
    }

    /// Register an existing index so we don't recommend duplicates.
    pub fn register_existing_index(&mut self, table: &str, columns: Vec<String>) {
        self.existing_indexes.push((table.to_string(), columns));
    }

    /// Generate index recommendations based on observed access patterns.
    pub fn recommend(&self) -> Vec<IndexRecommendation> {
        // Aggregate access frequency per (table, column)
        let mut freq_map: HashMap<(String, String), (u64, Vec<AccessType>)> = HashMap::new();
        for access in &self.accesses {
            let key = (access.table.clone(), access.column.clone());
            let entry = freq_map.entry(key).or_insert((0, Vec::new()));
            entry.0 += access.frequency;
            if !entry.1.contains(&access.access_type) {
                entry.1.push(access.access_type.clone());
            }
        }

        // Score each column: higher frequency + equality/join access = higher benefit
        let mut candidates: Vec<IndexRecommendation> = freq_map
            .iter()
            .filter(|((table, col), _)| {
                // Skip if an existing index already covers this column
                !self.existing_indexes.iter().any(|(t, cols)| {
                    t == table && cols.first().map(|c| c.as_str()) == Some(col.as_str())
                })
            })
            .map(|((table, col), (freq, access_types))| {
                let type_bonus: f64 = access_types
                    .iter()
                    .map(|at| match at {
                        AccessType::EqualityFilter => 1.0,
                        AccessType::Join => 0.9,
                        AccessType::RangeFilter => 0.7,
                        AccessType::OrderBy => 0.5,
                        AccessType::GroupBy => 0.4,
                    })
                    .sum();
                let score = (*freq as f64) * type_bonus;
                IndexRecommendation {
                    table: table.clone(),
                    columns: vec![col.clone()],
                    benefit_score: score,
                    reason: format!(
                        "Column '{}' accessed {} times with {:?}",
                        col, freq, access_types
                    ),
                }
            })
            .collect();

        candidates.sort_by(|a, b| {
            b.benefit_score
                .partial_cmp(&a.benefit_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(self.max_recommendations);
        candidates
    }

    /// Get total recorded accesses count.
    pub fn total_accesses(&self) -> usize {
        self.accesses.len()
    }

    /// Clear access history.
    pub fn clear_history(&mut self) {
        self.accesses.clear();
    }
}

// ─── Statistics Enhancer ─────────────────────────────────────────────

/// Enhanced statistics for a table column.
#[derive(Debug, Clone)]
pub struct ColumnStats {
    pub column_name: String,
    pub null_count: u64,
    pub distinct_count: u64,
    pub min_value: Option<String>,
    pub max_value: Option<String>,
    pub avg_length: f64,
    pub histogram: Vec<HistogramBucket>,
}

/// A histogram bucket for value distribution.
#[derive(Debug, Clone)]
pub struct HistogramBucket {
    pub lower_bound: String,
    pub upper_bound: String,
    pub row_count: u64,
    pub distinct_count: u64,
}

/// Table-level statistics.
#[derive(Debug, Clone)]
pub struct TableStats {
    pub table_name: String,
    pub row_count: u64,
    pub page_count: u64,
    pub avg_row_size: f64,
    pub last_analyzed_ms: u64,
    pub columns: HashMap<String, ColumnStats>,
}

/// Enhanced statistics collector and manager.
pub struct StatsEnhancer {
    tables: HashMap<String, TableStats>,
    sample_rate: f64,
    stale_threshold_ms: u64,
}

impl StatsEnhancer {
    pub fn new(sample_rate: f64, stale_threshold_ms: u64) -> Self {
        Self {
            tables: HashMap::new(),
            sample_rate: sample_rate.clamp(0.01, 1.0),
            stale_threshold_ms,
        }
    }

    /// Record statistics for a table.
    pub fn update_table_stats(&mut self, stats: TableStats) {
        self.tables.insert(stats.table_name.clone(), stats);
    }

    /// Add or update column statistics for a table.
    pub fn update_column_stats(&mut self, table: &str, col_stats: ColumnStats) -> bool {
        if let Some(ts) = self.tables.get_mut(table) {
            ts.columns.insert(col_stats.column_name.clone(), col_stats);
            true
        } else {
            false
        }
    }

    /// Get table statistics.
    pub fn get_table_stats(&self, table: &str) -> Option<&TableStats> {
        self.tables.get(table)
    }

    /// Get column statistics.
    pub fn get_column_stats(&self, table: &str, column: &str) -> Option<&ColumnStats> {
        self.tables.get(table)?.columns.get(column)
    }

    /// Estimate selectivity for an equality predicate on a column.
    pub fn estimate_selectivity(&self, table: &str, column: &str) -> f64 {
        if let Some(cs) = self.get_column_stats(table, column) {
            if cs.distinct_count > 0 {
                return 1.0 / cs.distinct_count as f64;
            }
        }
        // Default selectivity when no stats available
        0.1
    }

    /// Estimate row count after applying a filter on a column.
    pub fn estimate_filtered_rows(&self, table: &str, column: &str) -> u64 {
        let selectivity = self.estimate_selectivity(table, column);
        if let Some(ts) = self.tables.get(table) {
            (ts.row_count as f64 * selectivity).ceil() as u64
        } else {
            0
        }
    }

    /// Check if statistics for a table are stale.
    pub fn is_stale(&self, table: &str, current_time_ms: u64) -> bool {
        self.tables
            .get(table)
            .map(|ts| current_time_ms.saturating_sub(ts.last_analyzed_ms) > self.stale_threshold_ms)
            .unwrap_or(true) // No stats = stale
    }

    /// Get all tables with stale statistics.
    pub fn stale_tables(&self, current_time_ms: u64) -> Vec<&str> {
        self.tables
            .iter()
            .filter(|(_, ts)| {
                current_time_ms.saturating_sub(ts.last_analyzed_ms) > self.stale_threshold_ms
            })
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Get the configured sample rate.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Total tables with statistics.
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Add histogram buckets for a column.
    pub fn add_histogram(
        &mut self,
        table: &str,
        column: &str,
        buckets: Vec<HistogramBucket>,
    ) -> bool {
        if let Some(ts) = self.tables.get_mut(table) {
            if let Some(cs) = ts.columns.get_mut(column) {
                cs.histogram = buckets;
                return true;
            }
        }
        false
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_index_optimizer_best_index() {
        let mut opt = GlobalIndexOptimizer::new();
        opt.register_index(IndexDescriptor {
            index_name: "idx_users_name".into(),
            table_name: "users".into(),
            columns: vec!["name".into()],
            is_unique: false,
            is_covering: false,
            selectivity: 0.3,
        });
        opt.register_index(IndexDescriptor {
            index_name: "idx_users_email".into(),
            table_name: "users".into(),
            columns: vec!["email".into()],
            is_unique: true,
            is_covering: false,
            selectivity: 0.01,
        });

        let sel = opt.best_index("users", &["email"]).unwrap();
        assert_eq!(sel.index_name, "idx_users_email");
        assert!(sel.estimated_cost < 0.5); // very selective

        assert!(opt.best_index("users", &["nonexistent"]).is_none());
    }

    #[test]
    fn test_global_index_covering() {
        let mut opt = GlobalIndexOptimizer::new();
        opt.register_index(IndexDescriptor {
            index_name: "idx_orders_composite".into(),
            table_name: "orders".into(),
            columns: vec!["user_id".into(), "status".into(), "total".into()],
            is_unique: false,
            is_covering: true,
            selectivity: 0.2,
        });

        let covering = opt.find_covering_indexes("orders", &["user_id", "status"]);
        assert_eq!(covering.len(), 1);

        let not_covering = opt.find_covering_indexes("orders", &["user_id", "date"]);
        assert_eq!(not_covering.len(), 0);
    }

    #[test]
    fn test_query_rewriter_apply_rules() {
        let mut rw = QueryRewriter::new();
        rw.add_rule(RewriteRule {
            name: "fold_constants".into(),
            pattern: RewritePattern::ConstantFolding,
            priority: 100,
            enabled: true,
        });
        rw.add_rule(RewriteRule {
            name: "push_predicates".into(),
            pattern: RewritePattern::PredicatePushdown,
            priority: 90,
            enabled: true,
        });
        rw.add_rule(RewriteRule {
            name: "simplify_preds".into(),
            pattern: RewritePattern::PredicateSimplification,
            priority: 80,
            enabled: false,
        });

        let results = rw.apply_rules(&[
            RewritePattern::ConstantFolding,
            RewritePattern::PredicateSimplification,
        ]);

        // Only fold_constants should apply (simplify_preds is disabled)
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rule_name, "fold_constants");
        assert_eq!(rw.rule_applications("fold_constants"), 1);
    }

    #[test]
    fn test_query_rewriter_enable_disable() {
        let mut rw = QueryRewriter::new();
        rw.add_rule(RewriteRule {
            name: "r1".into(),
            pattern: RewritePattern::DistinctElimination,
            priority: 50,
            enabled: true,
        });
        assert_eq!(rw.enabled_rules().len(), 1);

        rw.set_rule_enabled("r1", false);
        assert_eq!(rw.enabled_rules().len(), 0);

        assert!(!rw.set_rule_enabled("nonexistent", true));
    }

    #[test]
    fn test_auto_index_advisor_recommend() {
        let mut advisor = AutoIndexAdvisor::new(3);
        advisor.record_access(ColumnAccess {
            table: "orders".into(),
            column: "user_id".into(),
            access_type: AccessType::EqualityFilter,
            frequency: 100,
        });
        advisor.record_access(ColumnAccess {
            table: "orders".into(),
            column: "status".into(),
            access_type: AccessType::RangeFilter,
            frequency: 50,
        });
        advisor.record_access(ColumnAccess {
            table: "orders".into(),
            column: "created_at".into(),
            access_type: AccessType::OrderBy,
            frequency: 200,
        });

        let recs = advisor.recommend();
        assert!(!recs.is_empty());
        // created_at has highest frequency (200 * 0.5 = 100)
        // user_id: 100 * 1.0 = 100
        // Both should be top
        assert!(recs.len() <= 3);
    }

    #[test]
    fn test_auto_index_advisor_skip_existing() {
        let mut advisor = AutoIndexAdvisor::new(5);
        advisor.register_existing_index("users", vec!["email".into()]);
        advisor.record_access(ColumnAccess {
            table: "users".into(),
            column: "email".into(),
            access_type: AccessType::EqualityFilter,
            frequency: 1000,
        });

        let recs = advisor.recommend();
        // Should skip 'email' since it already has an index
        assert!(recs.is_empty());
    }

    #[test]
    fn test_stats_enhancer_selectivity_estimation() {
        let mut se = StatsEnhancer::new(0.1, 3600000);
        let mut columns = HashMap::new();
        columns.insert(
            "status".to_string(),
            ColumnStats {
                column_name: "status".into(),
                null_count: 0,
                distinct_count: 5,
                min_value: Some("active".into()),
                max_value: Some("suspended".into()),
                avg_length: 8.0,
                histogram: vec![],
            },
        );
        se.update_table_stats(TableStats {
            table_name: "orders".into(),
            row_count: 10000,
            page_count: 500,
            avg_row_size: 128.0,
            last_analyzed_ms: 1000,
            columns,
        });

        let sel = se.estimate_selectivity("orders", "status");
        assert!((sel - 0.2).abs() < 0.001); // 1/5 = 0.2

        let filtered = se.estimate_filtered_rows("orders", "status");
        assert_eq!(filtered, 2000); // 10000 * 0.2
    }

    #[test]
    fn test_stats_enhancer_staleness() {
        let mut se = StatsEnhancer::new(0.1, 5000);
        se.update_table_stats(TableStats {
            table_name: "t1".into(),
            row_count: 100,
            page_count: 10,
            avg_row_size: 64.0,
            last_analyzed_ms: 1000,
            columns: HashMap::new(),
        });

        assert!(!se.is_stale("t1", 3000)); // 2000ms < 5000ms threshold
        assert!(se.is_stale("t1", 7000)); // 6000ms > 5000ms threshold
        assert!(se.is_stale("nonexistent", 1000)); // no stats = stale
    }

    #[test]
    fn test_stats_enhancer_histogram() {
        let mut se = StatsEnhancer::new(0.5, 60000);
        let mut columns = HashMap::new();
        columns.insert(
            "age".to_string(),
            ColumnStats {
                column_name: "age".into(),
                null_count: 5,
                distinct_count: 80,
                min_value: Some("1".into()),
                max_value: Some("100".into()),
                avg_length: 2.5,
                histogram: vec![],
            },
        );
        se.update_table_stats(TableStats {
            table_name: "people".into(),
            row_count: 5000,
            page_count: 200,
            avg_row_size: 96.0,
            last_analyzed_ms: 50000,
            columns,
        });

        let buckets = vec![
            HistogramBucket {
                lower_bound: "1".into(),
                upper_bound: "25".into(),
                row_count: 1200,
                distinct_count: 25,
            },
            HistogramBucket {
                lower_bound: "26".into(),
                upper_bound: "50".into(),
                row_count: 2000,
                distinct_count: 25,
            },
            HistogramBucket {
                lower_bound: "51".into(),
                upper_bound: "100".into(),
                row_count: 1800,
                distinct_count: 30,
            },
        ];
        assert!(se.add_histogram("people", "age", buckets));
        let cs = se.get_column_stats("people", "age").unwrap();
        assert_eq!(cs.histogram.len(), 3);
        assert_eq!(cs.histogram[1].row_count, 2000);
    }
}
