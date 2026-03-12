// R12 – Performance counters, slow query log, and plan cache hit metrics.
//
// Provides:
//   - `PerfCounters`: atomic counters for key database operations
//   - `SlowQueryLog`: captures SQL statements exceeding a time threshold
//   - `PlanCacheStats`: tracks plan cache hit/miss/eviction rates

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

// ── Performance Counters ──────────────────────────────────────────────

/// Atomic performance counters for database operations.
pub struct PerfCounters {
    pub queries_executed: AtomicU64,
    pub rows_read: AtomicU64,
    pub rows_written: AtomicU64,
    pub bytes_read: AtomicU64,
    pub bytes_written: AtomicU64,
    pub index_lookups: AtomicU64,
    pub full_scans: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub transactions_committed: AtomicU64,
    pub transactions_aborted: AtomicU64,
    pub lock_waits: AtomicU64,
    pub deadlocks_detected: AtomicU64,
}

impl Default for PerfCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl PerfCounters {
    pub fn new() -> Self {
        Self {
            queries_executed: AtomicU64::new(0),
            rows_read: AtomicU64::new(0),
            rows_written: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            index_lookups: AtomicU64::new(0),
            full_scans: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            transactions_committed: AtomicU64::new(0),
            transactions_aborted: AtomicU64::new(0),
            lock_waits: AtomicU64::new(0),
            deadlocks_detected: AtomicU64::new(0),
        }
    }

    pub fn inc_queries(&self) {
        self.queries_executed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_rows_read(&self, n: u64) {
        self.rows_read.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_rows_written(&self, n: u64) {
        self.rows_written.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_bytes_read(&self, n: u64) {
        self.bytes_read.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_bytes_written(&self, n: u64) {
        self.bytes_written.fetch_add(n, Ordering::Relaxed);
    }

    pub fn inc_index_lookups(&self) {
        self.index_lookups.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_full_scans(&self) {
        self.full_scans.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_tx_committed(&self) {
        self.transactions_committed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_tx_aborted(&self) {
        self.transactions_aborted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_lock_waits(&self) {
        self.lock_waits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_deadlocks(&self) {
        self.deadlocks_detected.fetch_add(1, Ordering::Relaxed);
    }

    /// Cache hit ratio [0.0, 1.0].
    pub fn cache_hit_ratio(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed) as f64;
        let misses = self.cache_misses.load(Ordering::Relaxed) as f64;
        let total = hits + misses;
        if total == 0.0 {
            0.0
        } else {
            hits / total
        }
    }

    /// Get a snapshot of all counters.
    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            queries_executed: self.queries_executed.load(Ordering::Relaxed),
            rows_read: self.rows_read.load(Ordering::Relaxed),
            rows_written: self.rows_written.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            index_lookups: self.index_lookups.load(Ordering::Relaxed),
            full_scans: self.full_scans.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            transactions_committed: self.transactions_committed.load(Ordering::Relaxed),
            transactions_aborted: self.transactions_aborted.load(Ordering::Relaxed),
            lock_waits: self.lock_waits.load(Ordering::Relaxed),
            deadlocks_detected: self.deadlocks_detected.load(Ordering::Relaxed),
        }
    }

    /// Reset all counters.
    pub fn reset(&self) {
        self.queries_executed.store(0, Ordering::Relaxed);
        self.rows_read.store(0, Ordering::Relaxed);
        self.rows_written.store(0, Ordering::Relaxed);
        self.bytes_read.store(0, Ordering::Relaxed);
        self.bytes_written.store(0, Ordering::Relaxed);
        self.index_lookups.store(0, Ordering::Relaxed);
        self.full_scans.store(0, Ordering::Relaxed);
        self.cache_hits.store(0, Ordering::Relaxed);
        self.cache_misses.store(0, Ordering::Relaxed);
        self.transactions_committed.store(0, Ordering::Relaxed);
        self.transactions_aborted.store(0, Ordering::Relaxed);
        self.lock_waits.store(0, Ordering::Relaxed);
        self.deadlocks_detected.store(0, Ordering::Relaxed);
    }
}

/// Immutable snapshot of performance counters.
#[derive(Debug, Clone)]
pub struct PerfSnapshot {
    pub queries_executed: u64,
    pub rows_read: u64,
    pub rows_written: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub index_lookups: u64,
    pub full_scans: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub transactions_committed: u64,
    pub transactions_aborted: u64,
    pub lock_waits: u64,
    pub deadlocks_detected: u64,
}

// ── Slow Query Log ────────────────────────────────────────────────────

/// A single slow query entry.
#[derive(Debug, Clone)]
pub struct SlowQueryEntry {
    pub sql: String,
    pub duration: Duration,
    pub rows_examined: u64,
    pub rows_returned: u64,
    pub timestamp: SystemTime,
}

/// Slow query log — captures queries that exceed a configured time threshold.
pub struct SlowQueryLog {
    threshold: Duration,
    entries: VecDeque<SlowQueryEntry>,
    max_entries: usize,
    total_slow: u64,
}

impl SlowQueryLog {
    pub fn new(threshold: Duration, max_entries: usize) -> Self {
        Self {
            threshold,
            entries: VecDeque::new(),
            max_entries,
            total_slow: 0,
        }
    }

    /// Record a query execution. If duration >= threshold, it's logged.
    /// Returns `true` if the query was logged as slow.
    pub fn record(
        &mut self,
        sql: &str,
        duration: Duration,
        rows_examined: u64,
        rows_returned: u64,
    ) -> bool {
        if duration < self.threshold {
            return false;
        }
        self.total_slow += 1;
        let entry = SlowQueryEntry {
            sql: sql.to_string(),
            duration,
            rows_examined,
            rows_returned,
            timestamp: SystemTime::now(),
        };
        self.entries.push_back(entry);
        while self.entries.len() > self.max_entries {
            self.entries.pop_front();
        }
        true
    }

    /// Get all entries.
    pub fn entries(&self) -> &VecDeque<SlowQueryEntry> {
        &self.entries
    }

    /// Number of entries currently stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total slow queries recorded (including evicted ones).
    pub fn total_slow(&self) -> u64 {
        self.total_slow
    }

    /// Current threshold.
    pub fn threshold(&self) -> Duration {
        self.threshold
    }

    /// Update the slow query threshold.
    pub fn set_threshold(&mut self, threshold: Duration) {
        self.threshold = threshold;
    }

    /// Find the slowest query.
    pub fn slowest(&self) -> Option<&SlowQueryEntry> {
        self.entries.iter().max_by_key(|e| e.duration)
    }

    /// Average duration of logged slow queries.
    pub fn avg_duration(&self) -> Option<Duration> {
        if self.entries.is_empty() {
            return None;
        }
        let total: Duration = self.entries.iter().map(|e| e.duration).sum();
        Some(total / self.entries.len() as u32)
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drain all entries.
    pub fn drain(&mut self) -> Vec<SlowQueryEntry> {
        self.entries.drain(..).collect()
    }
}

// ── Plan Cache Stats ──────────────────────────────────────────────────

/// Tracks plan cache hit/miss/eviction statistics.
pub struct PlanCacheStats {
    hits: u64,
    misses: u64,
    evictions: u64,
    inserts: u64,
}

impl Default for PlanCacheStats {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanCacheStats {
    pub fn new() -> Self {
        Self {
            hits: 0,
            misses: 0,
            evictions: 0,
            inserts: 0,
        }
    }

    pub fn record_hit(&mut self) {
        self.hits += 1;
    }

    pub fn record_miss(&mut self) {
        self.misses += 1;
    }

    pub fn record_eviction(&mut self) {
        self.evictions += 1;
    }

    pub fn record_insert(&mut self) {
        self.inserts += 1;
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    pub fn inserts(&self) -> u64 {
        self.inserts
    }

    /// Hit ratio [0.0, 1.0].
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Eviction ratio relative to inserts.
    pub fn eviction_ratio(&self) -> f64 {
        if self.inserts == 0 {
            0.0
        } else {
            self.evictions as f64 / self.inserts as f64
        }
    }

    /// Reset all stats.
    pub fn reset(&mut self) {
        self.hits = 0;
        self.misses = 0;
        self.evictions = 0;
        self.inserts = 0;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_counters_basic() {
        let c = PerfCounters::new();
        c.inc_queries();
        c.inc_queries();
        c.add_rows_read(100);
        c.inc_cache_hit();
        c.inc_cache_miss();
        let snap = c.snapshot();
        assert_eq!(snap.queries_executed, 2);
        assert_eq!(snap.rows_read, 100);
        assert!((c.cache_hit_ratio() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn perf_counters_reset() {
        let c = PerfCounters::new();
        c.inc_queries();
        c.inc_tx_committed();
        c.reset();
        let snap = c.snapshot();
        assert_eq!(snap.queries_executed, 0);
        assert_eq!(snap.transactions_committed, 0);
    }

    #[test]
    fn perf_counters_cache_ratio_empty() {
        let c = PerfCounters::new();
        assert_eq!(c.cache_hit_ratio(), 0.0);
    }

    #[test]
    fn slow_query_log_basic() {
        let mut log = SlowQueryLog::new(Duration::from_millis(100), 10);
        let fast = log.record("SELECT 1", Duration::from_millis(5), 1, 1);
        assert!(!fast);
        let slow = log.record("SELECT * FROM big", Duration::from_millis(500), 10000, 5000);
        assert!(slow);
        assert_eq!(log.len(), 1);
        assert_eq!(log.total_slow(), 1);
    }

    #[test]
    fn slow_query_log_eviction() {
        let mut log = SlowQueryLog::new(Duration::from_millis(1), 3);
        for i in 0..5 {
            log.record(&format!("Q{i}"), Duration::from_millis(10), 0, 0);
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.total_slow(), 5);
    }

    #[test]
    fn slow_query_log_slowest() {
        let mut log = SlowQueryLog::new(Duration::from_millis(1), 10);
        log.record("Q1", Duration::from_millis(10), 0, 0);
        log.record("Q2", Duration::from_millis(500), 0, 0);
        log.record("Q3", Duration::from_millis(50), 0, 0);
        let slowest = log.slowest().unwrap();
        assert_eq!(slowest.sql, "Q2");
    }

    #[test]
    fn slow_query_log_avg_duration() {
        let mut log = SlowQueryLog::new(Duration::from_millis(1), 10);
        log.record("Q1", Duration::from_millis(100), 0, 0);
        log.record("Q2", Duration::from_millis(200), 0, 0);
        let avg = log.avg_duration().unwrap();
        assert_eq!(avg, Duration::from_millis(150));
    }

    #[test]
    fn plan_cache_stats_basic() {
        let mut s = PlanCacheStats::new();
        s.record_hit();
        s.record_hit();
        s.record_miss();
        s.record_insert();
        s.record_eviction();
        assert_eq!(s.hits(), 2);
        assert_eq!(s.misses(), 1);
        assert!((s.hit_ratio() - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.eviction_ratio() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plan_cache_stats_empty() {
        let s = PlanCacheStats::new();
        assert_eq!(s.hit_ratio(), 0.0);
        assert_eq!(s.eviction_ratio(), 0.0);
    }

    #[test]
    fn plan_cache_stats_reset() {
        let mut s = PlanCacheStats::new();
        s.record_hit();
        s.record_miss();
        s.reset();
        assert_eq!(s.hits(), 0);
        assert_eq!(s.misses(), 0);
    }
}
