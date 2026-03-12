// ── src/vm/monitor/observability_v2.rs ──
// R21: 系统可观测性 — 分布式追踪 / 指标聚合 / 健康检查仪表盘 / 告警规则引擎

use std::collections::{HashMap, VecDeque};

// ═══════════════════════════════════════════════════════════════════════
// 1. DistributedTracer — 分布式追踪
// ═══════════════════════════════════════════════════════════════════════

/// 追踪 Span
#[derive(Debug, Clone)]
pub struct TraceSpan {
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_span_id: Option<u64>,
    pub operation: String,
    pub service: String,
    pub start_us: u64,
    pub duration_us: u64,
    pub tags: HashMap<String, String>,
    pub status: SpanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanStatus {
    Ok,
    Error,
    Timeout,
}

/// 分布式追踪器
pub struct DistributedTracer {
    spans: Vec<TraceSpan>,
    next_trace_id: u64,
    next_span_id: u64,
    max_spans: usize,
}

impl DistributedTracer {
    pub fn new(max_spans: usize) -> Self {
        Self {
            spans: Vec::new(),
            next_trace_id: 1,
            next_span_id: 1,
            max_spans,
        }
    }

    pub fn start_trace(&mut self, operation: &str, service: &str) -> (u64, u64) {
        let trace_id = self.next_trace_id;
        self.next_trace_id += 1;
        let span_id = self.start_span(trace_id, None, operation, service);
        (trace_id, span_id)
    }

    pub fn start_span(
        &mut self,
        trace_id: u64,
        parent: Option<u64>,
        operation: &str,
        service: &str,
    ) -> u64 {
        let span_id = self.next_span_id;
        self.next_span_id += 1;

        if self.spans.len() >= self.max_spans {
            self.spans.remove(0);
        }

        self.spans.push(TraceSpan {
            trace_id,
            span_id,
            parent_span_id: parent,
            operation: operation.to_string(),
            service: service.to_string(),
            start_us: 0,
            duration_us: 0,
            tags: HashMap::new(),
            status: SpanStatus::Ok,
        });
        span_id
    }

    pub fn finish_span(&mut self, span_id: u64, duration_us: u64, status: SpanStatus) {
        if let Some(span) = self.spans.iter_mut().find(|s| s.span_id == span_id) {
            span.duration_us = duration_us;
            span.status = status;
        }
    }

    pub fn add_tag(&mut self, span_id: u64, key: &str, value: &str) {
        if let Some(span) = self.spans.iter_mut().find(|s| s.span_id == span_id) {
            span.tags.insert(key.to_string(), value.to_string());
        }
    }

    pub fn get_trace(&self, trace_id: u64) -> Vec<&TraceSpan> {
        self.spans
            .iter()
            .filter(|s| s.trace_id == trace_id)
            .collect()
    }

    pub fn error_spans(&self) -> Vec<&TraceSpan> {
        self.spans
            .iter()
            .filter(|s| s.status == SpanStatus::Error)
            .collect()
    }

    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    pub fn trace_count(&self) -> usize {
        let mut ids: Vec<u64> = self.spans.iter().map(|s| s.trace_id).collect();
        ids.sort();
        ids.dedup();
        ids.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. MetricsAggregator — 指标聚合器
// ═══════════════════════════════════════════════════════════════════════

/// 指标类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricType {
    Counter,
    Gauge,
    Histogram,
}

/// 指标值
#[derive(Debug, Clone)]
pub struct MetricValue {
    pub name: String,
    pub metric_type: MetricType,
    pub value: f64,
    pub labels: HashMap<String, String>,
    pub timestamp_ms: u64,
}

/// 聚合窗口
#[derive(Debug, Clone)]
pub struct AggWindow {
    pub values: VecDeque<f64>,
    pub max_size: usize,
}

impl AggWindow {
    pub fn new(max_size: usize) -> Self {
        Self {
            values: VecDeque::new(),
            max_size,
        }
    }

    pub fn push(&mut self, val: f64) {
        if self.values.len() >= self.max_size {
            self.values.pop_front();
        }
        self.values.push_back(val);
    }

    pub fn avg(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }

    pub fn min(&self) -> f64 {
        self.values.iter().cloned().fold(f64::MAX, f64::min)
    }

    pub fn max(&self) -> f64 {
        self.values.iter().cloned().fold(f64::MIN, f64::max)
    }

    pub fn p99(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.values.iter().cloned().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((sorted.len() as f64) * 0.99).ceil() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
}

/// 指标聚合器
pub struct MetricsAggregator {
    counters: HashMap<String, f64>,
    gauges: HashMap<String, f64>,
    windows: HashMap<String, AggWindow>,
    window_size: usize,
}

impl MetricsAggregator {
    pub fn new(window_size: usize) -> Self {
        Self {
            counters: HashMap::new(),
            gauges: HashMap::new(),
            windows: HashMap::new(),
            window_size,
        }
    }

    pub fn inc_counter(&mut self, name: &str, delta: f64) {
        *self.counters.entry(name.to_string()).or_insert(0.0) += delta;
    }

    pub fn set_gauge(&mut self, name: &str, value: f64) {
        self.gauges.insert(name.to_string(), value);
    }

    pub fn observe(&mut self, name: &str, value: f64) {
        let window = self
            .windows
            .entry(name.to_string())
            .or_insert_with(|| AggWindow::new(self.window_size));
        window.push(value);
    }

    pub fn get_counter(&self, name: &str) -> f64 {
        self.counters.get(name).copied().unwrap_or(0.0)
    }

    pub fn get_gauge(&self, name: &str) -> f64 {
        self.gauges.get(name).copied().unwrap_or(0.0)
    }

    pub fn get_window(&self, name: &str) -> Option<&AggWindow> {
        self.windows.get(name)
    }

    pub fn metric_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        names.extend(self.counters.keys().cloned());
        names.extend(self.gauges.keys().cloned());
        names.extend(self.windows.keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    pub fn total_metrics(&self) -> usize {
        self.counters.len() + self.gauges.len() + self.windows.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. HealthDashboard — 健康检查仪表盘
// ═══════════════════════════════════════════════════════════════════════

/// 健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

/// 组件健康
#[derive(Debug, Clone)]
pub struct ComponentHealth {
    pub name: String,
    pub state: HealthState,
    pub message: String,
    pub last_check_ms: u64,
    pub consecutive_failures: u32,
}

/// 健康仪表盘
pub struct HealthDashboard {
    components: HashMap<String, ComponentHealth>,
    #[allow(dead_code)]
    check_interval_ms: u64,
    failure_threshold: u32,
}

impl HealthDashboard {
    pub fn new(check_interval_ms: u64, failure_threshold: u32) -> Self {
        Self {
            components: HashMap::new(),
            check_interval_ms,
            failure_threshold,
        }
    }

    pub fn register(&mut self, name: &str) {
        self.components.insert(
            name.to_string(),
            ComponentHealth {
                name: name.to_string(),
                state: HealthState::Unknown,
                message: String::new(),
                last_check_ms: 0,
                consecutive_failures: 0,
            },
        );
    }

    pub fn report_healthy(&mut self, name: &str, message: &str, timestamp_ms: u64) {
        if let Some(c) = self.components.get_mut(name) {
            c.state = HealthState::Healthy;
            c.message = message.to_string();
            c.last_check_ms = timestamp_ms;
            c.consecutive_failures = 0;
        }
    }

    pub fn report_failure(&mut self, name: &str, message: &str, timestamp_ms: u64) {
        if let Some(c) = self.components.get_mut(name) {
            c.consecutive_failures += 1;
            c.message = message.to_string();
            c.last_check_ms = timestamp_ms;
            if c.consecutive_failures >= self.failure_threshold {
                c.state = HealthState::Unhealthy;
            } else {
                c.state = HealthState::Degraded;
            }
        }
    }

    pub fn overall_state(&self) -> HealthState {
        if self
            .components
            .values()
            .any(|c| c.state == HealthState::Unhealthy)
        {
            HealthState::Unhealthy
        } else if self
            .components
            .values()
            .any(|c| c.state == HealthState::Degraded || c.state == HealthState::Unknown)
        {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        }
    }

    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    pub fn unhealthy_components(&self) -> Vec<&str> {
        self.components
            .values()
            .filter(|c| c.state == HealthState::Unhealthy)
            .map(|c| c.name.as_str())
            .collect()
    }

    pub fn summary(&self) -> HashMap<String, String> {
        self.components
            .iter()
            .map(|(name, c)| (name.clone(), format!("{:?}: {}", c.state, c.message)))
            .collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. AlertRuleEngine — 告警规则引擎
// ═══════════════════════════════════════════════════════════════════════

/// 告警级别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Info,
    Warning,
    Critical,
    Emergency,
}

/// 告警条件
#[derive(Debug, Clone)]
pub enum AlertCondition {
    ThresholdAbove(f64),
    ThresholdBelow(f64),
    RateOfChange(f64), // change per second
    Absent(u64),       // no data for N ms
}

/// 告警规则
#[derive(Debug, Clone)]
pub struct AlertRule {
    pub id: u32,
    pub name: String,
    pub metric: String,
    pub condition: AlertCondition,
    pub level: AlertLevel,
    pub enabled: bool,
    pub cooldown_ms: u64,
    pub last_fired_ms: u64,
}

/// 触发的告警
#[derive(Debug, Clone)]
pub struct FiredAlert {
    pub rule_id: u32,
    pub rule_name: String,
    pub level: AlertLevel,
    pub value: f64,
    pub message: String,
    pub timestamp_ms: u64,
}

/// 告警规则引擎
pub struct AlertRuleEngine {
    rules: Vec<AlertRule>,
    fired: VecDeque<FiredAlert>,
    max_history: usize,
    next_rule_id: u32,
    total_fired: u64,
}

impl AlertRuleEngine {
    pub fn new(max_history: usize) -> Self {
        Self {
            rules: Vec::new(),
            fired: VecDeque::new(),
            max_history,
            next_rule_id: 1,
            total_fired: 0,
        }
    }

    pub fn add_rule(
        &mut self,
        name: &str,
        metric: &str,
        condition: AlertCondition,
        level: AlertLevel,
        cooldown_ms: u64,
    ) -> u32 {
        let id = self.next_rule_id;
        self.next_rule_id += 1;
        self.rules.push(AlertRule {
            id,
            name: name.to_string(),
            metric: metric.to_string(),
            condition,
            level,
            enabled: true,
            cooldown_ms,
            last_fired_ms: 0,
        });
        id
    }

    pub fn disable_rule(&mut self, rule_id: u32) {
        if let Some(r) = self.rules.iter_mut().find(|r| r.id == rule_id) {
            r.enabled = false;
        }
    }

    /// 评估指标值
    pub fn evaluate(&mut self, metric: &str, value: f64, timestamp_ms: u64) -> Vec<FiredAlert> {
        let mut alerts = Vec::new();

        for rule in &mut self.rules {
            if !rule.enabled || rule.metric != metric {
                continue;
            }
            if rule.last_fired_ms > 0 && timestamp_ms < rule.last_fired_ms + rule.cooldown_ms {
                continue;
            }

            let triggered = match &rule.condition {
                AlertCondition::ThresholdAbove(t) => value > *t,
                AlertCondition::ThresholdBelow(t) => value < *t,
                AlertCondition::RateOfChange(max_rate) => value.abs() > *max_rate,
                AlertCondition::Absent(_) => false, // needs special handling
            };

            if triggered {
                rule.last_fired_ms = timestamp_ms;
                let alert = FiredAlert {
                    rule_id: rule.id,
                    rule_name: rule.name.clone(),
                    level: rule.level,
                    value,
                    message: format!("{}: {} = {:.2}", rule.name, metric, value),
                    timestamp_ms,
                };
                alerts.push(alert);
            }
        }

        for a in &alerts {
            if self.fired.len() >= self.max_history {
                self.fired.pop_front();
            }
            self.fired.push_back(a.clone());
            self.total_fired += 1;
        }

        alerts
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn active_rule_count(&self) -> usize {
        self.rules.iter().filter(|r| r.enabled).count()
    }

    pub fn fired_history(&self) -> &VecDeque<FiredAlert> {
        &self.fired
    }

    pub fn total_fired(&self) -> u64 {
        self.total_fired
    }

    pub fn critical_alerts(&self) -> Vec<&FiredAlert> {
        self.fired
            .iter()
            .filter(|a| a.level == AlertLevel::Critical || a.level == AlertLevel::Emergency)
            .collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracer_trace_lifecycle() {
        let mut tracer = DistributedTracer::new(1000);
        let (tid, root_sid) = tracer.start_trace("SELECT", "query-engine");
        let child_sid = tracer.start_span(tid, Some(root_sid), "scan", "storage");
        tracer.add_tag(child_sid, "table", "users");
        tracer.finish_span(child_sid, 500, SpanStatus::Ok);
        tracer.finish_span(root_sid, 1000, SpanStatus::Ok);

        let trace = tracer.get_trace(tid);
        assert_eq!(trace.len(), 2);
        assert_eq!(tracer.trace_count(), 1);
    }

    #[test]
    fn test_tracer_error_spans() {
        let mut tracer = DistributedTracer::new(100);
        let (_tid, sid) = tracer.start_trace("INSERT", "vm");
        tracer.finish_span(sid, 100, SpanStatus::Error);
        assert_eq!(tracer.error_spans().len(), 1);
    }

    #[test]
    fn test_metrics_aggregator() {
        let mut agg = MetricsAggregator::new(100);
        agg.inc_counter("queries_total", 1.0);
        agg.inc_counter("queries_total", 1.0);
        assert_eq!(agg.get_counter("queries_total"), 2.0);

        agg.set_gauge("connections", 42.0);
        assert_eq!(agg.get_gauge("connections"), 42.0);

        for i in 0..10 {
            agg.observe("query_latency_us", i as f64 * 100.0);
        }
        let w = agg.get_window("query_latency_us").unwrap();
        assert_eq!(w.min(), 0.0);
        assert_eq!(w.max(), 900.0);
        assert!((w.avg() - 450.0).abs() < 0.1);
    }

    #[test]
    fn test_agg_window_p99() {
        let mut w = AggWindow::new(100);
        for i in 1..=100 {
            w.push(i as f64);
        }
        assert!((w.p99() - 99.0).abs() <= 1.0);
    }

    #[test]
    fn test_health_dashboard() {
        let mut dash = HealthDashboard::new(5000, 3);
        dash.register("storage");
        dash.register("raft");
        dash.register("query_engine");

        dash.report_healthy("storage", "OK", 1000);
        dash.report_healthy("raft", "OK", 1000);
        dash.report_healthy("query_engine", "OK", 1000);
        assert_eq!(dash.overall_state(), HealthState::Healthy);

        dash.report_failure("raft", "leader timeout", 2000);
        assert_eq!(dash.overall_state(), HealthState::Degraded);

        dash.report_failure("raft", "leader timeout", 3000);
        dash.report_failure("raft", "leader timeout", 4000);
        assert_eq!(dash.overall_state(), HealthState::Unhealthy);
        assert_eq!(dash.unhealthy_components(), vec!["raft"]);
    }

    #[test]
    fn test_health_summary() {
        let mut dash = HealthDashboard::new(1000, 5);
        dash.register("vm");
        dash.report_healthy("vm", "all good", 100);
        let s = dash.summary();
        assert!(s.get("vm").unwrap().contains("Healthy"));
    }

    #[test]
    fn test_alert_rule_engine_threshold() {
        let mut engine = AlertRuleEngine::new(100);
        engine.add_rule(
            "high_cpu",
            "cpu_percent",
            AlertCondition::ThresholdAbove(90.0),
            AlertLevel::Critical,
            0,
        );
        engine.add_rule(
            "low_memory",
            "free_mem_mb",
            AlertCondition::ThresholdBelow(100.0),
            AlertLevel::Warning,
            0,
        );

        let alerts = engine.evaluate("cpu_percent", 95.0, 1000);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].level, AlertLevel::Critical);

        let alerts2 = engine.evaluate("free_mem_mb", 50.0, 2000);
        assert_eq!(alerts2.len(), 1);
        assert_eq!(alerts2[0].level, AlertLevel::Warning);

        assert_eq!(engine.total_fired(), 2);
    }

    #[test]
    fn test_alert_cooldown() {
        let mut engine = AlertRuleEngine::new(50);
        engine.add_rule(
            "hot",
            "temp",
            AlertCondition::ThresholdAbove(100.0),
            AlertLevel::Warning,
            5000,
        );

        let a1 = engine.evaluate("temp", 120.0, 1000);
        assert_eq!(a1.len(), 1);
        // Within cooldown
        let a2 = engine.evaluate("temp", 120.0, 3000);
        assert!(a2.is_empty());
        // After cooldown
        let a3 = engine.evaluate("temp", 120.0, 7000);
        assert_eq!(a3.len(), 1);
    }

    #[test]
    fn test_alert_disable_rule() {
        let mut engine = AlertRuleEngine::new(50);
        let rid = engine.add_rule(
            "test",
            "x",
            AlertCondition::ThresholdAbove(0.0),
            AlertLevel::Info,
            0,
        );
        engine.disable_rule(rid);
        let alerts = engine.evaluate("x", 100.0, 1000);
        assert!(alerts.is_empty());
        assert_eq!(engine.active_rule_count(), 0);
    }

    #[test]
    fn test_alert_critical_filter() {
        let mut engine = AlertRuleEngine::new(100);
        engine.add_rule(
            "warn",
            "metric",
            AlertCondition::ThresholdAbove(50.0),
            AlertLevel::Warning,
            0,
        );
        engine.add_rule(
            "crit",
            "metric",
            AlertCondition::ThresholdAbove(80.0),
            AlertLevel::Critical,
            0,
        );
        engine.evaluate("metric", 90.0, 1000);
        let crits = engine.critical_alerts();
        assert_eq!(crits.len(), 1);
    }
}
