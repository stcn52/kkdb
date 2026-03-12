// R17 – Observability & Operations:
//   - Slow query collector
//   - Resource watermark alerting
//   - Connection pool monitoring
//   - Lock wait visualization
//   - Online config hot-reload
//
// Provides:
//   - `SlowQueryCollector`: captures and ranks slow queries
//   - `ResourceWatermark`: threshold-based resource alerting
//   - `ConnPoolMonitor`: tracks connection pool utilization
//   - `LockWaitGraph`: visualizes lock waits for deadlock analysis
//   - `HotConfigReload`: live configuration hot-reload

use std::collections::HashMap;

// ── Slow Query Collector ──────────────────────────────────────────────

/// A captured slow query.
#[derive(Debug, Clone)]
pub struct SlowQuery {
    pub sql: String,
    pub duration_us: u64,
    pub timestamp: u64,
    pub rows_examined: u64,
    pub rows_returned: u64,
}

/// Collects and analyzes slow queries.
pub struct SlowQueryCollector {
    threshold_us: u64,
    queries: Vec<SlowQuery>,
    max_entries: usize,
}

impl SlowQueryCollector {
    pub fn new(threshold_us: u64, max_entries: usize) -> Self {
        Self { threshold_us, queries: Vec::new(), max_entries }
    }

    /// Record a query execution. Only stores if above threshold.
    pub fn record(&mut self, sql: &str, duration_us: u64, ts: u64, examined: u64, returned: u64) -> bool {
        if duration_us < self.threshold_us { return false; }
        if self.queries.len() >= self.max_entries {
            // Evict the fastest slow query
            if let Some(min_idx) = self.queries.iter()
                .enumerate()
                .min_by_key(|(_, q)| q.duration_us)
                .map(|(i, _)| i) {
                if self.queries[min_idx].duration_us < duration_us {
                    self.queries.swap_remove(min_idx);
                } else {
                    return false;
                }
            }
        }
        self.queries.push(SlowQuery {
            sql: sql.to_string(),
            duration_us,
            timestamp: ts,
            rows_examined: examined,
            rows_returned: returned,
        });
        true
    }

    /// Top N slowest queries.
    pub fn top_n(&self, n: usize) -> Vec<&SlowQuery> {
        let mut sorted: Vec<&SlowQuery> = self.queries.iter().collect();
        sorted.sort_by(|a, b| b.duration_us.cmp(&a.duration_us));
        sorted.truncate(n);
        sorted
    }

    /// Average duration of captured queries.
    pub fn avg_duration_us(&self) -> f64 {
        if self.queries.is_empty() { return 0.0; }
        let sum: u64 = self.queries.iter().map(|q| q.duration_us).sum();
        sum as f64 / self.queries.len() as f64
    }

    /// Queries with high examined-to-returned ratio (inefficient scans).
    pub fn inefficient_queries(&self, ratio_threshold: f64) -> Vec<&SlowQuery> {
        self.queries.iter()
            .filter(|q| {
                if q.rows_returned == 0 { return q.rows_examined > 0; }
                (q.rows_examined as f64 / q.rows_returned as f64) > ratio_threshold
            })
            .collect()
    }

    pub fn count(&self) -> usize {
        self.queries.len()
    }

    pub fn set_threshold(&mut self, threshold_us: u64) {
        self.threshold_us = threshold_us;
    }
}

// ── Resource Watermark Alerting ───────────────────────────────────────

/// Alert level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertLevel {
    Normal,
    Warning,
    Critical,
}

/// A resource metric reading.
#[derive(Debug, Clone)]
pub struct ResourceReading {
    pub name: String,
    pub current_value: f64,
    pub warning_threshold: f64,
    pub critical_threshold: f64,
}

impl ResourceReading {
    pub fn alert_level(&self) -> AlertLevel {
        if self.current_value >= self.critical_threshold {
            AlertLevel::Critical
        } else if self.current_value >= self.warning_threshold {
            AlertLevel::Warning
        } else {
            AlertLevel::Normal
        }
    }
}

/// Resource watermark alerting.
pub struct ResourceWatermark {
    resources: HashMap<String, ResourceReading>,
    alert_history: Vec<(String, AlertLevel, u64)>,
}

impl ResourceWatermark {
    pub fn new() -> Self {
        Self { resources: HashMap::new(), alert_history: Vec::new() }
    }

    pub fn register(&mut self, name: &str, warning: f64, critical: f64) {
        self.resources.insert(name.to_string(), ResourceReading {
            name: name.to_string(),
            current_value: 0.0,
            warning_threshold: warning,
            critical_threshold: critical,
        });
    }

    pub fn update(&mut self, name: &str, value: f64, ts: u64) {
        if let Some(r) = self.resources.get_mut(name) {
            r.current_value = value;
            let level = r.alert_level();
            if level != AlertLevel::Normal {
                self.alert_history.push((name.to_string(), level, ts));
            }
        }
    }

    pub fn get_level(&self, name: &str) -> AlertLevel {
        self.resources.get(name).map(|r| r.alert_level()).unwrap_or(AlertLevel::Normal)
    }

    /// All resources at warning or above.
    pub fn active_alerts(&self) -> Vec<(&str, AlertLevel)> {
        self.resources.values()
            .filter(|r| r.alert_level() != AlertLevel::Normal)
            .map(|r| (r.name.as_str(), r.alert_level()))
            .collect()
    }

    pub fn alert_history_count(&self) -> usize {
        self.alert_history.len()
    }

    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }
}

// ── Connection Pool Monitor ───────────────────────────────────────────

/// Connection state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnState {
    Idle,
    Active,
    Waiting,
}

/// A monitored connection.
#[derive(Debug, Clone)]
pub struct MonitoredConn {
    pub conn_id: u64,
    pub state: ConnState,
    pub user: String,
    pub active_since: u64,
    pub query: Option<String>,
}

/// Connection pool utilization tracker.
pub struct ConnPoolMonitor {
    connections: HashMap<u64, MonitoredConn>,
    max_connections: usize,
    peak_active: usize,
}

impl ConnPoolMonitor {
    pub fn new(max_connections: usize) -> Self {
        Self { connections: HashMap::new(), max_connections, peak_active: 0 }
    }

    pub fn add_connection(&mut self, conn_id: u64, user: &str) {
        self.connections.insert(conn_id, MonitoredConn {
            conn_id,
            state: ConnState::Idle,
            user: user.to_string(),
            active_since: 0,
            query: None,
        });
    }

    pub fn set_active(&mut self, conn_id: u64, query: &str, ts: u64) {
        if let Some(c) = self.connections.get_mut(&conn_id) {
            c.state = ConnState::Active;
            c.active_since = ts;
            c.query = Some(query.to_string());
        }
        let active = self.active_count();
        if active > self.peak_active { self.peak_active = active; }
    }

    pub fn set_idle(&mut self, conn_id: u64) {
        if let Some(c) = self.connections.get_mut(&conn_id) {
            c.state = ConnState::Idle;
            c.query = None;
        }
    }

    pub fn remove_connection(&mut self, conn_id: u64) {
        self.connections.remove(&conn_id);
    }

    pub fn active_count(&self) -> usize {
        self.connections.values().filter(|c| c.state == ConnState::Active).count()
    }

    pub fn idle_count(&self) -> usize {
        self.connections.values().filter(|c| c.state == ConnState::Idle).count()
    }

    pub fn total_count(&self) -> usize {
        self.connections.len()
    }

    pub fn utilization(&self) -> f64 {
        if self.max_connections == 0 { return 0.0; }
        self.active_count() as f64 / self.max_connections as f64
    }

    pub fn peak_active(&self) -> usize {
        self.peak_active
    }

    /// Long-running queries (active longer than threshold).
    pub fn long_running(&self, current_ts: u64, threshold_s: u64) -> Vec<&MonitoredConn> {
        self.connections.values()
            .filter(|c| c.state == ConnState::Active
                && current_ts.saturating_sub(c.active_since) > threshold_s)
            .collect()
    }
}

// ── Lock Wait Visualization ───────────────────────────────────────────

/// A lock wait edge: txn_waiting -> txn_holding.
#[derive(Debug, Clone)]
pub struct LockWaitEdge {
    pub waiter_txn: u64,
    pub holder_txn: u64,
    pub resource: String,
    pub wait_start_ts: u64,
}

/// Lock wait graph for visualization & deadlock analysis.
pub struct LockWaitGraph {
    edges: Vec<LockWaitEdge>,
}

impl LockWaitGraph {
    pub fn new() -> Self {
        Self { edges: Vec::new() }
    }

    pub fn add_wait(&mut self, waiter: u64, holder: u64, resource: &str, ts: u64) {
        self.edges.push(LockWaitEdge {
            waiter_txn: waiter,
            holder_txn: holder,
            resource: resource.to_string(),
            wait_start_ts: ts,
        });
    }

    pub fn remove_wait(&mut self, waiter: u64, holder: u64) {
        self.edges.retain(|e| !(e.waiter_txn == waiter && e.holder_txn == holder));
    }

    /// Detect cycles (simple DFS-based).
    pub fn detect_cycles(&self) -> Vec<Vec<u64>> {
        let mut adj: HashMap<u64, Vec<u64>> = HashMap::new();
        for e in &self.edges {
            adj.entry(e.waiter_txn).or_default().push(e.holder_txn);
        }
        let mut cycles = Vec::new();
        let mut visited = HashMap::new();
        let nodes: Vec<u64> = adj.keys().copied().collect();
        for &start in &nodes {
            if visited.get(&start).copied().unwrap_or(0) == 2 { continue; }
            let mut path = Vec::new();
            Self::dfs(start, &adj, &mut visited, &mut path, &mut cycles);
        }
        cycles
    }

    fn dfs(
        node: u64,
        adj: &HashMap<u64, Vec<u64>>,
        visited: &mut HashMap<u64, u8>,
        path: &mut Vec<u64>,
        cycles: &mut Vec<Vec<u64>>,
    ) {
        if visited.get(&node).copied().unwrap_or(0) == 1 {
            // Found cycle
            if let Some(pos) = path.iter().position(|&n| n == node) {
                cycles.push(path[pos..].to_vec());
            }
            return;
        }
        if visited.get(&node).copied().unwrap_or(0) == 2 { return; }
        visited.insert(node, 1);
        path.push(node);
        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                Self::dfs(next, adj, visited, path, cycles);
            }
        }
        path.pop();
        visited.insert(node, 2);
    }

    /// Render graph as text for visualization.
    pub fn render_text(&self) -> String {
        let mut lines = Vec::new();
        lines.push("Lock Wait Graph:".to_string());
        for e in &self.edges {
            lines.push(format!(
                "  txn:{} --[{}]--> txn:{}",
                e.waiter_txn, e.resource, e.holder_txn
            ));
        }
        lines.join("\n")
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

// ── Online Config Hot-Reload ──────────────────────────────────────────

/// Config parameter with versioning.
#[derive(Debug, Clone)]
pub struct ConfigParam {
    pub key: String,
    pub value: String,
    pub version: u64,
    pub is_dynamic: bool,
}

/// Hot-reloadable configuration system.
pub struct HotConfigReload {
    params: HashMap<String, ConfigParam>,
    version: u64,
    change_log: Vec<(String, String, String, u64)>, // key, old, new, version
}

impl HotConfigReload {
    pub fn new() -> Self {
        Self { params: HashMap::new(), version: 0, change_log: Vec::new() }
    }

    pub fn register(&mut self, key: &str, value: &str, is_dynamic: bool) {
        self.params.insert(key.to_string(), ConfigParam {
            key: key.to_string(),
            value: value.to_string(),
            version: self.version,
            is_dynamic,
        });
    }

    /// Update a config parameter. Returns Ok(old_value) on success.
    pub fn update(&mut self, key: &str, new_value: &str) -> Result<String, String> {
        let param = self.params.get_mut(key)
            .ok_or_else(|| format!("unknown config: {}", key))?;
        if !param.is_dynamic {
            return Err(format!("{} is not dynamically reloadable", key));
        }
        let old = param.value.clone();
        self.version += 1;
        param.value = new_value.to_string();
        param.version = self.version;
        self.change_log.push((key.to_string(), old.clone(), new_value.to_string(), self.version));
        Ok(old)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(|p| p.value.as_str())
    }

    pub fn is_dynamic(&self, key: &str) -> Option<bool> {
        self.params.get(key).map(|p| p.is_dynamic)
    }

    pub fn current_version(&self) -> u64 {
        self.version
    }

    pub fn change_count(&self) -> usize {
        self.change_log.len()
    }

    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    /// Get all dynamic params.
    pub fn dynamic_params(&self) -> Vec<&str> {
        self.params.values()
            .filter(|p| p.is_dynamic)
            .map(|p| p.key.as_str())
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_query_collector_top_n() {
        let mut sq = SlowQueryCollector::new(100, 100);
        sq.record("SELECT 1", 50, 1, 10, 1); // below threshold
        sq.record("SELECT * FROM big", 500, 2, 10000, 10);
        sq.record("SELECT * FROM huge", 2000, 3, 100000, 5);
        assert_eq!(sq.count(), 2);
        let top = sq.top_n(1);
        assert_eq!(top[0].duration_us, 2000);
    }

    #[test]
    fn slow_query_inefficient() {
        let mut sq = SlowQueryCollector::new(100, 100);
        sq.record("SELECT * FROM t WHERE x=1", 200, 1, 10000, 1);
        let ineff = sq.inefficient_queries(100.0);
        assert_eq!(ineff.len(), 1);
    }

    #[test]
    fn resource_watermark_alerts() {
        let mut rw = ResourceWatermark::new();
        rw.register("cpu", 70.0, 90.0);
        rw.register("memory", 80.0, 95.0);
        rw.update("cpu", 85.0, 1);
        rw.update("memory", 50.0, 1);
        assert_eq!(rw.get_level("cpu"), AlertLevel::Warning);
        assert_eq!(rw.get_level("memory"), AlertLevel::Normal);
        let alerts = rw.active_alerts();
        assert_eq!(alerts.len(), 1);
    }

    #[test]
    fn conn_pool_monitor_utilization() {
        let mut cpm = ConnPoolMonitor::new(100);
        cpm.add_connection(1, "admin");
        cpm.add_connection(2, "user");
        cpm.set_active(1, "SELECT 1", 100);
        assert_eq!(cpm.active_count(), 1);
        assert_eq!(cpm.idle_count(), 1);
        assert!((cpm.utilization() - 0.01).abs() < 0.001);
        assert_eq!(cpm.peak_active(), 1);
    }

    #[test]
    fn conn_pool_long_running() {
        let mut cpm = ConnPoolMonitor::new(100);
        cpm.add_connection(1, "admin");
        cpm.set_active(1, "ANALYZE TABLE t", 10);
        let long = cpm.long_running(100, 30);
        assert_eq!(long.len(), 1);
    }

    #[test]
    fn lock_wait_graph_cycle_detection() {
        let mut lwg = LockWaitGraph::new();
        lwg.add_wait(1, 2, "table_a", 100);
        lwg.add_wait(2, 3, "table_b", 101);
        lwg.add_wait(3, 1, "table_c", 102);
        let cycles = lwg.detect_cycles();
        assert!(!cycles.is_empty());
    }

    #[test]
    fn lock_wait_graph_render() {
        let mut lwg = LockWaitGraph::new();
        lwg.add_wait(1, 2, "users", 100);
        let text = lwg.render_text();
        assert!(text.contains("txn:1"));
        assert!(text.contains("txn:2"));
    }

    #[test]
    fn hot_config_reload_dynamic() {
        let mut hc = HotConfigReload::new();
        hc.register("max_connections", "100", true);
        hc.register("data_dir", "/var/data", false);
        let old = hc.update("max_connections", "200").unwrap();
        assert_eq!(old, "100");
        assert_eq!(hc.get("max_connections"), Some("200"));
        assert!(hc.update("data_dir", "/tmp").is_err()); // not dynamic
        assert_eq!(hc.current_version(), 1);
    }

    #[test]
    fn hot_config_dynamic_params() {
        let mut hc = HotConfigReload::new();
        hc.register("a", "1", true);
        hc.register("b", "2", false);
        hc.register("c", "3", true);
        let dynamic = hc.dynamic_params();
        assert_eq!(dynamic.len(), 2);
    }
}
