// R14 – Observability & ops tooling: query tracing, resource quotas,
//       online DDL progress tracking, auto statistics update.
//
// Provides:
//   - `QueryTracer` + `TraceSpan`: distributed query tracing
//   - `ResourceQuota` + `QuotaManager`: per-user/per-db resource limits
//   - `DdlProgressTracker`: online DDL operation progress
//   - `AutoStatsUpdater`: automatic statistics refresh scheduler

use std::collections::HashMap;
use std::time::{Duration, Instant};

// ── Query Tracing ─────────────────────────────────────────────────────

/// A span in a query trace.
#[derive(Debug, Clone)]
pub struct TraceSpan {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub operation: String,
    pub start: Instant,
    pub duration: Option<Duration>,
    pub metadata: HashMap<String, String>,
}

impl TraceSpan {
    pub fn new(span_id: u64, operation: &str) -> Self {
        Self {
            span_id,
            parent_id: None,
            operation: operation.to_string(),
            start: Instant::now(),
            duration: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_parent(mut self, parent_id: u64) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    pub fn finish(&mut self) {
        self.duration = Some(self.start.elapsed());
    }

    pub fn set_metadata(&mut self, key: &str, value: &str) {
        self.metadata.insert(key.to_string(), value.to_string());
    }

    pub fn is_finished(&self) -> bool {
        self.duration.is_some()
    }
}

/// Collects trace spans for a single query execution.
pub struct QueryTracer {
    pub trace_id: u64,
    pub query: String,
    spans: Vec<TraceSpan>,
    next_span_id: u64,
}

impl QueryTracer {
    pub fn new(trace_id: u64, query: &str) -> Self {
        Self {
            trace_id,
            query: query.to_string(),
            spans: Vec::new(),
            next_span_id: 1,
        }
    }

    /// Start a new span. Returns its span_id.
    pub fn start_span(&mut self, operation: &str, parent_id: Option<u64>) -> u64 {
        let id = self.next_span_id;
        self.next_span_id += 1;
        let mut span = TraceSpan::new(id, operation);
        if let Some(pid) = parent_id {
            span = span.with_parent(pid);
        }
        self.spans.push(span);
        id
    }

    /// Finish a span.
    pub fn finish_span(&mut self, span_id: u64) {
        if let Some(span) = self.spans.iter_mut().find(|s| s.span_id == span_id) {
            span.finish();
        }
    }

    /// Add metadata to a span.
    pub fn set_span_metadata(&mut self, span_id: u64, key: &str, value: &str) {
        if let Some(span) = self.spans.iter_mut().find(|s| s.span_id == span_id) {
            span.set_metadata(key, value);
        }
    }

    /// Total trace duration.
    pub fn total_duration(&self) -> Duration {
        self.spans
            .iter()
            .filter_map(|s| s.duration)
            .max()
            .unwrap_or(Duration::ZERO)
    }

    /// Get all spans.
    pub fn spans(&self) -> &[TraceSpan] {
        &self.spans
    }

    /// Find the slowest span.
    pub fn slowest_span(&self) -> Option<&TraceSpan> {
        self.spans
            .iter()
            .filter(|s| s.duration.is_some())
            .max_by_key(|s| s.duration.unwrap())
    }

    pub fn span_count(&self) -> usize {
        self.spans.len()
    }
}

// ── Resource Quotas ───────────────────────────────────────────────────

/// Resource limits for a user or database.
#[derive(Debug, Clone)]
pub struct ResourceQuota {
    pub name: String,
    pub max_concurrent_queries: u32,
    pub max_memory_bytes: u64,
    pub max_query_time: Duration,
    pub max_result_rows: u64,
}

impl ResourceQuota {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            max_concurrent_queries: u32::MAX,
            max_memory_bytes: u64::MAX,
            max_query_time: Duration::from_secs(3600),
            max_result_rows: u64::MAX,
        }
    }

    pub fn with_concurrent_queries(mut self, max: u32) -> Self {
        self.max_concurrent_queries = max;
        self
    }

    pub fn with_memory(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    pub fn with_query_time(mut self, d: Duration) -> Self {
        self.max_query_time = d;
        self
    }

    pub fn with_result_rows(mut self, max: u64) -> Self {
        self.max_result_rows = max;
        self
    }
}

/// Current resource usage for a user/db.
#[derive(Debug, Clone, Default)]
pub struct ResourceUsage {
    pub active_queries: u32,
    pub memory_bytes: u64,
}

/// Manages quotas and checks usage.
pub struct QuotaManager {
    quotas: HashMap<String, ResourceQuota>,
    usage: HashMap<String, ResourceUsage>,
}

impl Default for QuotaManager {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaManager {
    pub fn new() -> Self {
        Self {
            quotas: HashMap::new(),
            usage: HashMap::new(),
        }
    }

    pub fn set_quota(&mut self, quota: ResourceQuota) {
        self.quotas.insert(quota.name.clone(), quota);
    }

    pub fn remove_quota(&mut self, name: &str) -> bool {
        self.quotas.remove(name).is_some()
    }

    /// Check if the user can start a new query.
    pub fn can_start_query(&self, name: &str) -> bool {
        let quota = match self.quotas.get(name) {
            Some(q) => q,
            None => return true, // no quota → allow
        };
        let usage = self.usage.get(name);
        let active = usage.map(|u| u.active_queries).unwrap_or(0);
        active < quota.max_concurrent_queries
    }

    /// Check if memory usage is within quota.
    pub fn check_memory(&self, name: &str, additional_bytes: u64) -> bool {
        let quota = match self.quotas.get(name) {
            Some(q) => q,
            None => return true,
        };
        let current = self.usage.get(name).map(|u| u.memory_bytes).unwrap_or(0);
        current + additional_bytes <= quota.max_memory_bytes
    }

    /// Record that a query started.
    pub fn query_started(&mut self, name: &str) {
        let usage = self.usage.entry(name.to_string()).or_default();
        usage.active_queries += 1;
    }

    /// Record that a query finished.
    pub fn query_finished(&mut self, name: &str) {
        if let Some(usage) = self.usage.get_mut(name) {
            usage.active_queries = usage.active_queries.saturating_sub(1);
        }
    }

    /// Update memory usage.
    pub fn update_memory(&mut self, name: &str, bytes: u64) {
        let usage = self.usage.entry(name.to_string()).or_default();
        usage.memory_bytes = bytes;
    }

    pub fn quota_count(&self) -> usize {
        self.quotas.len()
    }
}

// ── DDL Progress Tracker ──────────────────────────────────────────────

/// State of an online DDL operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlState {
    Pending,
    Preparing,
    CopyingData,
    SwappingTables,
    CleaningUp,
    Completed,
    Failed,
}

impl std::fmt::Display for DdlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "PENDING"),
            Self::Preparing => write!(f, "PREPARING"),
            Self::CopyingData => write!(f, "COPYING DATA"),
            Self::SwappingTables => write!(f, "SWAPPING TABLES"),
            Self::CleaningUp => write!(f, "CLEANING UP"),
            Self::Completed => write!(f, "COMPLETED"),
            Self::Failed => write!(f, "FAILED"),
        }
    }
}

/// Progress of an online DDL operation.
#[derive(Debug, Clone)]
pub struct DdlProgress {
    pub operation_id: u64,
    pub ddl_sql: String,
    pub state: DdlState,
    pub total_rows: u64,
    pub processed_rows: u64,
    pub started_at: Instant,
    pub error: Option<String>,
}

impl DdlProgress {
    pub fn new(operation_id: u64, ddl_sql: &str, total_rows: u64) -> Self {
        Self {
            operation_id,
            ddl_sql: ddl_sql.to_string(),
            state: DdlState::Pending,
            total_rows,
            processed_rows: 0,
            started_at: Instant::now(),
            error: None,
        }
    }

    /// Progress percentage.
    pub fn percent_complete(&self) -> f64 {
        if self.total_rows == 0 {
            return if self.state == DdlState::Completed {
                100.0
            } else {
                0.0
            };
        }
        (self.processed_rows as f64 / self.total_rows as f64) * 100.0
    }

    /// Estimated time remaining.
    pub fn eta(&self) -> Option<Duration> {
        if self.processed_rows == 0 {
            return None;
        }
        let elapsed = self.started_at.elapsed();
        let rate = self.processed_rows as f64 / elapsed.as_secs_f64();
        if rate <= 0.0 {
            return None;
        }
        let remaining = (self.total_rows - self.processed_rows) as f64 / rate;
        Some(Duration::from_secs_f64(remaining))
    }

    pub fn advance(&mut self, rows: u64) {
        self.processed_rows = (self.processed_rows + rows).min(self.total_rows);
    }

    pub fn set_state(&mut self, state: DdlState) {
        self.state = state;
    }

    pub fn fail(&mut self, error: &str) {
        self.state = DdlState::Failed;
        self.error = Some(error.to_string());
    }
}

/// Tracks all active DDL operations.
pub struct DdlProgressTracker {
    operations: HashMap<u64, DdlProgress>,
    next_id: u64,
}

impl Default for DdlProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DdlProgressTracker {
    pub fn new() -> Self {
        Self {
            operations: HashMap::new(),
            next_id: 1,
        }
    }

    /// Start tracking a new DDL operation.
    pub fn start(&mut self, ddl_sql: &str, total_rows: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.operations
            .insert(id, DdlProgress::new(id, ddl_sql, total_rows));
        id
    }

    pub fn get(&self, id: u64) -> Option<&DdlProgress> {
        self.operations.get(&id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut DdlProgress> {
        self.operations.get_mut(&id)
    }

    /// Remove completed or failed operations.
    pub fn cleanup(&mut self) -> usize {
        let before = self.operations.len();
        self.operations
            .retain(|_, p| p.state != DdlState::Completed && p.state != DdlState::Failed);
        before - self.operations.len()
    }

    pub fn active_count(&self) -> usize {
        self.operations.len()
    }
}

// ── Auto Statistics Updater ───────────────────────────────────────────

/// Configuration for auto-stats refresh.
#[derive(Debug, Clone)]
pub struct StatsRefreshConfig {
    pub table_name: String,
    /// Refresh if more than this fraction of rows changed.
    pub change_threshold: f64,
    /// Minimum interval between refreshes.
    pub min_interval: Duration,
}

/// Tracks table modification counts to decide when to refresh statistics.
pub struct AutoStatsUpdater {
    configs: HashMap<String, StatsRefreshConfig>,
    /// table_name → (rows_changed, last_refresh, total_rows)
    state: HashMap<String, (u64, Instant, u64)>,
}

impl Default for AutoStatsUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoStatsUpdater {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            state: HashMap::new(),
        }
    }

    /// Register a table for auto-stats.
    pub fn register(&mut self, config: StatsRefreshConfig) {
        let name = config.table_name.clone();
        self.configs.insert(name.clone(), config);
        self.state.entry(name).or_insert((0, Instant::now(), 0));
    }

    /// Set the total row count for a table.
    pub fn set_row_count(&mut self, table: &str, total: u64) {
        if let Some(s) = self.state.get_mut(table) {
            s.2 = total;
        }
    }

    /// Record that rows were modified.
    pub fn record_modification(&mut self, table: &str, rows: u64) {
        if let Some(s) = self.state.get_mut(table) {
            s.0 += rows;
        }
    }

    /// Check if a table needs statistics refresh.
    pub fn needs_refresh(&self, table: &str) -> bool {
        let config = match self.configs.get(table) {
            Some(c) => c,
            None => return false,
        };
        let state = match self.state.get(table) {
            Some(s) => s,
            None => return false,
        };
        let (changed, last_refresh, total) = state;
        if last_refresh.elapsed() < config.min_interval {
            return false;
        }
        if *total == 0 {
            return *changed > 0;
        }
        (*changed as f64 / *total as f64) > config.change_threshold
    }

    /// Mark that statistics were refreshed.
    pub fn mark_refreshed(&mut self, table: &str) {
        if let Some(s) = self.state.get_mut(table) {
            s.0 = 0;
            s.1 = Instant::now();
        }
    }

    /// List all tables needing refresh.
    pub fn tables_needing_refresh(&self) -> Vec<String> {
        self.configs
            .keys()
            .filter(|t| self.needs_refresh(t))
            .cloned()
            .collect()
    }

    pub fn table_count(&self) -> usize {
        self.configs.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_tracer_spans() {
        let mut tracer = QueryTracer::new(1, "SELECT * FROM t1");
        let s1 = tracer.start_span("parse", None);
        tracer.finish_span(s1);
        let s2 = tracer.start_span("optimize", None);
        tracer.set_span_metadata(s2, "strategy", "cost-based");
        tracer.finish_span(s2);
        assert_eq!(tracer.span_count(), 2);
        assert!(tracer.spans()[0].is_finished());
    }

    #[test]
    fn query_tracer_slowest() {
        let mut tracer = QueryTracer::new(1, "SELECT 1");
        let s1 = tracer.start_span("fast", None);
        tracer.finish_span(s1);
        let s2 = tracer.start_span("slow", None);
        std::thread::sleep(Duration::from_millis(5));
        tracer.finish_span(s2);
        let slowest = tracer.slowest_span().unwrap();
        assert_eq!(slowest.operation, "slow");
    }

    #[test]
    fn resource_quota_check() {
        let mut qm = QuotaManager::new();
        qm.set_quota(
            ResourceQuota::new("alice")
                .with_concurrent_queries(2)
                .with_memory(1_000_000),
        );
        assert!(qm.can_start_query("alice"));
        qm.query_started("alice");
        qm.query_started("alice");
        assert!(!qm.can_start_query("alice")); // at limit
        qm.query_finished("alice");
        assert!(qm.can_start_query("alice"));
    }

    #[test]
    fn resource_quota_memory() {
        let mut qm = QuotaManager::new();
        qm.set_quota(ResourceQuota::new("bob").with_memory(1000));
        qm.update_memory("bob", 500);
        assert!(qm.check_memory("bob", 400)); // 500 + 400 = 900 ≤ 1000
        assert!(!qm.check_memory("bob", 600)); // 500 + 600 = 1100 > 1000
    }

    #[test]
    fn ddl_progress_tracking() {
        let mut tracker = DdlProgressTracker::new();
        let id = tracker.start("ALTER TABLE t1 ADD COLUMN c2 INT", 1000);
        let p = tracker.get_mut(id).unwrap();
        p.set_state(DdlState::CopyingData);
        p.advance(500);
        assert!((p.percent_complete() - 50.0).abs() < 0.1);
        p.advance(500);
        p.set_state(DdlState::Completed);
        assert!((p.percent_complete() - 100.0).abs() < 0.1);
        let cleaned = tracker.cleanup();
        assert_eq!(cleaned, 1);
    }

    #[test]
    fn ddl_progress_failure() {
        let mut p = DdlProgress::new(1, "DROP INDEX idx1", 0);
        p.fail("index not found");
        assert_eq!(p.state, DdlState::Failed);
        assert!(p.error.is_some());
    }

    #[test]
    fn ddl_state_display() {
        assert_eq!(format!("{}", DdlState::CopyingData), "COPYING DATA");
        assert_eq!(format!("{}", DdlState::Completed), "COMPLETED");
    }

    #[test]
    fn auto_stats_needs_refresh() {
        let mut updater = AutoStatsUpdater::new();
        updater.register(StatsRefreshConfig {
            table_name: "users".into(),
            change_threshold: 0.1,
            min_interval: Duration::from_millis(0), // no minimum
        });
        updater.set_row_count("users", 1000);
        updater.record_modification("users", 50); // 5% < 10%
        assert!(!updater.needs_refresh("users"));
        updater.record_modification("users", 60); // 11% > 10%
        assert!(updater.needs_refresh("users"));
        updater.mark_refreshed("users");
        assert!(!updater.needs_refresh("users"));
    }

    #[test]
    fn auto_stats_tables_needing_refresh() {
        let mut updater = AutoStatsUpdater::new();
        updater.register(StatsRefreshConfig {
            table_name: "t1".into(),
            change_threshold: 0.05,
            min_interval: Duration::from_millis(0),
        });
        updater.register(StatsRefreshConfig {
            table_name: "t2".into(),
            change_threshold: 0.05,
            min_interval: Duration::from_millis(0),
        });
        updater.set_row_count("t1", 100);
        updater.set_row_count("t2", 100);
        updater.record_modification("t1", 10); // 10% > 5%
        let tables = updater.tables_needing_refresh();
        assert!(tables.contains(&"t1".to_string()));
        assert!(!tables.contains(&"t2".to_string()));
    }

    #[test]
    fn query_tracer_with_parent_span() {
        let mut tracer = QueryTracer::new(1, "SELECT 1");
        let root = tracer.start_span("execute", None);
        let child = tracer.start_span("scan", Some(root));
        tracer.finish_span(child);
        tracer.finish_span(root);
        assert_eq!(tracer.spans()[1].parent_id, Some(root));
    }
}
