// ── Query Cache ─────────────────────────────────────────────────────────────
//
// MySQL-style query cache: caches the result of identical SELECT queries.
// When a table is modified (INSERT/UPDATE/DELETE/DROP), all entries referencing
// that table are invalidated automatically.
//
// The cache uses LRU eviction when the entry count exceeds `max_entries`.
// Entries are keyed by the normalized SQL string.
//
// ## Usage
//
// 1. Before executing a SELECT, call `get(sql)`.
//    - On hit: return cached result directly.
//    - On miss: execute the query, then call `put(sql, tables, result)`.
//
// 2. After any DML/DDL on a table, call `invalidate_table(table_name)`.
//
// 3. To flush everything: `clear()`.

use crate::types::Value;
use std::collections::{HashMap, VecDeque};

/// A single cached query result.
#[derive(Debug, Clone)]
pub struct QueryCacheEntry {
    /// Column names of the result set.
    pub columns: Vec<String>,
    /// Rows of the result set.
    pub rows: Vec<Vec<Value>>,
    /// Which tables this query reads from (lowercase).
    /// Used for invalidation on DML.
    pub referenced_tables: Vec<String>,
    /// Number of times this entry has been served from cache.
    pub hit_count: u64,
}

/// LRU query cache with table-level invalidation.
#[derive(Debug, Clone)]
pub struct QueryCache {
    /// SQL string → cached result.
    entries: HashMap<String, QueryCacheEntry>,
    /// LRU order: front = oldest, back = most recently used.
    lru_order: VecDeque<String>,
    /// Maximum number of entries before LRU eviction.
    max_entries: usize,
    /// Whether the cache is enabled.
    enabled: bool,
    // ── Statistics ────────────────────────────────────────────────
    /// Total cache lookups (get calls).
    pub stat_lookups: u64,
    /// Total cache hits.
    pub stat_hits: u64,
    /// Total cache misses.
    pub stat_misses: u64,
    /// Total cache insertions.
    pub stat_inserts: u64,
    /// Total cache invalidations (entries removed due to DML).
    pub stat_invalidations: u64,
    /// Total LRU evictions.
    pub stat_evictions: u64,
}

impl Default for QueryCache {
    fn default() -> Self {
        Self::new(256)
    }
}

impl QueryCache {
    /// Create a new query cache with the given capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru_order: VecDeque::new(),
            max_entries,
            enabled: true,
            stat_lookups: 0,
            stat_hits: 0,
            stat_misses: 0,
            stat_inserts: 0,
            stat_invalidations: 0,
            stat_evictions: 0,
        }
    }

    /// Look up a cached result by SQL string.
    /// Returns `Some((columns, rows))` on hit, `None` on miss.
    pub fn get(&mut self, sql: &str) -> Option<(Vec<String>, Vec<Vec<Value>>)> {
        self.stat_lookups += 1;
        let key = Self::normalize_key(sql);
        if let Some(entry) = self.entries.get_mut(&key) {
            self.stat_hits += 1;
            entry.hit_count += 1;
            // Move to back of LRU
            self.lru_order.retain(|k| k != &key);
            self.lru_order.push_back(key);
            Some((entry.columns.clone(), entry.rows.clone()))
        } else {
            self.stat_misses += 1;
            None
        }
    }

    /// Insert a query result into the cache.
    ///
    /// `sql` — the original SQL string.
    /// `referenced_tables` — list of table names this query reads from (lowercase).
    /// `columns` / `rows` — the query result.
    pub fn put(
        &mut self,
        sql: &str,
        referenced_tables: Vec<String>,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    ) {
        if !self.enabled {
            return;
        }

        let key = Self::normalize_key(sql);

        // Evict if at capacity
        while self.entries.len() >= self.max_entries && !self.lru_order.is_empty() {
            if let Some(oldest) = self.lru_order.pop_front() {
                self.entries.remove(&oldest);
                self.stat_evictions += 1;
            }
        }

        // Remove old entry from LRU if replacing
        self.lru_order.retain(|k| k != &key);
        self.lru_order.push_back(key.clone());

        self.entries.insert(
            key,
            QueryCacheEntry {
                columns,
                rows,
                referenced_tables,
                hit_count: 0,
            },
        );
        self.stat_inserts += 1;
    }

    /// Invalidate all cached entries that reference the given table.
    /// Called after INSERT, UPDATE, DELETE, DROP TABLE, ALTER TABLE, etc.
    pub fn invalidate_table(&mut self, table_name: &str) {
        let table_lower = table_name.to_ascii_lowercase();
        let keys_to_remove: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry
                    .referenced_tables
                    .iter()
                    .any(|t| t == &table_lower)
            })
            .map(|(k, _)| k.clone())
            .collect();

        for key in &keys_to_remove {
            self.entries.remove(key);
            self.lru_order.retain(|k| k != key);
            self.stat_invalidations += 1;
        }
    }

    /// Clear the entire cache.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru_order.clear();
    }

    /// Reset statistics counters.
    pub fn reset_stats(&mut self) {
        self.stat_lookups = 0;
        self.stat_hits = 0;
        self.stat_misses = 0;
        self.stat_inserts = 0;
        self.stat_invalidations = 0;
        self.stat_evictions = 0;
    }

    /// Enable or disable the cache.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.clear();
        }
    }

    /// Whether the cache is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Maximum capacity.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Set maximum capacity. If smaller than current size, evicts immediately.
    pub fn set_max_entries(&mut self, max_entries: usize) {
        self.max_entries = max_entries;
        while self.entries.len() > self.max_entries && !self.lru_order.is_empty() {
            if let Some(oldest) = self.lru_order.pop_front() {
                self.entries.remove(&oldest);
                self.stat_evictions += 1;
            }
        }
    }

    /// Compute hit rate as a percentage (0.0–100.0).
    pub fn hit_rate(&self) -> f64 {
        if self.stat_lookups == 0 {
            return 0.0;
        }
        (self.stat_hits as f64 / self.stat_lookups as f64) * 100.0
    }

    /// Normalize SQL for use as cache key.
    /// Trims whitespace and converts to lowercase.
    fn normalize_key(sql: &str) -> String {
        sql.trim().to_ascii_lowercase()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_put_and_get() {
        let mut cache = QueryCache::new(10);
        cache.put(
            "SELECT * FROM t",
            vec!["t".into()],
            vec!["id".into(), "name".into()],
            vec![vec![Value::Integer(1), Value::Text("a".into())]],
        );

        let result = cache.get("SELECT * FROM t");
        assert!(result.is_some());
        let (cols, rows) = result.unwrap();
        assert_eq!(cols, vec!["id", "name"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], Value::Integer(1));
    }

    #[test]
    fn test_cache_miss() {
        let mut cache = QueryCache::new(10);
        assert!(cache.get("SELECT 1").is_none());
        assert_eq!(cache.stat_misses, 1);
    }

    #[test]
    fn test_cache_normalize_key() {
        let mut cache = QueryCache::new(10);
        cache.put(
            "  SELECT * FROM t  ",
            vec!["t".into()],
            vec!["id".into()],
            vec![vec![Value::Integer(1)]],
        );

        // Should hit even with different whitespace/case
        assert!(cache.get("select * from t").is_some());
    }

    #[test]
    fn test_cache_invalidation() {
        let mut cache = QueryCache::new(10);
        cache.put(
            "SELECT * FROM users",
            vec!["users".into()],
            vec!["id".into()],
            vec![vec![Value::Integer(1)]],
        );
        cache.put(
            "SELECT * FROM orders",
            vec!["orders".into()],
            vec!["id".into()],
            vec![vec![Value::Integer(2)]],
        );

        assert_eq!(cache.len(), 2);

        // Invalidate users table
        cache.invalidate_table("users");
        assert_eq!(cache.len(), 1);
        assert!(cache.get("SELECT * FROM users").is_none());
        assert!(cache.get("SELECT * FROM orders").is_some());
    }

    #[test]
    fn test_cache_invalidation_multi_table() {
        let mut cache = QueryCache::new(10);
        // Query that joins users and orders
        cache.put(
            "SELECT * FROM users JOIN orders",
            vec!["users".into(), "orders".into()],
            vec!["uid".into(), "oid".into()],
            vec![],
        );

        // Invalidating either table should remove it
        cache.invalidate_table("orders");
        assert!(cache.get("SELECT * FROM users JOIN orders").is_none());
    }

    #[test]
    fn test_cache_lru_eviction() {
        let mut cache = QueryCache::new(3);

        cache.put("q1", vec!["t".into()], vec!["c".into()], vec![]);
        cache.put("q2", vec!["t".into()], vec!["c".into()], vec![]);
        cache.put("q3", vec!["t".into()], vec!["c".into()], vec![]);
        assert_eq!(cache.len(), 3);

        // Adding q4 should evict q1 (LRU)
        cache.put("q4", vec!["t".into()], vec!["c".into()], vec![]);
        assert_eq!(cache.len(), 3);
        assert!(cache.get("q1").is_none()); // evicted
        assert!(cache.get("q2").is_some());
    }

    #[test]
    fn test_cache_lru_access_refreshes() {
        let mut cache = QueryCache::new(3);

        cache.put("q1", vec!["t".into()], vec!["c".into()], vec![]);
        cache.put("q2", vec!["t".into()], vec!["c".into()], vec![]);
        cache.put("q3", vec!["t".into()], vec!["c".into()], vec![]);

        // Access q1 to refresh it
        cache.get("q1");

        // Now q2 is the LRU, should be evicted
        cache.put("q4", vec!["t".into()], vec!["c".into()], vec![]);
        assert!(cache.get("q1").is_some()); // was refreshed
        assert!(cache.get("q2").is_none()); // evicted
    }

    #[test]
    fn test_cache_stats() {
        let mut cache = QueryCache::new(10);
        cache.put(
            "SELECT 1",
            vec![],
            vec!["1".into()],
            vec![vec![Value::Integer(1)]],
        );

        cache.get("SELECT 1"); // hit
        cache.get("SELECT 1"); // hit
        cache.get("SELECT 2"); // miss

        assert_eq!(cache.stat_inserts, 1);
        assert_eq!(cache.stat_hits, 2);
        assert_eq!(cache.stat_misses, 1);
        assert_eq!(cache.stat_lookups, 3);
        assert!((cache.hit_rate() - 66.666).abs() < 1.0);
    }

    #[test]
    fn test_cache_disable() {
        let mut cache = QueryCache::new(10);
        cache.put("q1", vec!["t".into()], vec!["c".into()], vec![]);
        assert_eq!(cache.len(), 1);

        cache.set_enabled(false);
        assert_eq!(cache.len(), 0); // cleared
        assert!(!cache.is_enabled());

        // Puts are ignored when disabled
        cache.put("q2", vec!["t".into()], vec!["c".into()], vec![]);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_cache_clear() {
        let mut cache = QueryCache::new(10);
        cache.put("q1", vec!["t".into()], vec!["c".into()], vec![]);
        cache.put("q2", vec!["t".into()], vec!["c".into()], vec![]);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_set_max_entries_shrinks() {
        let mut cache = QueryCache::new(10);
        for i in 0..10 {
            cache.put(&format!("q{}", i), vec!["t".into()], vec!["c".into()], vec![]);
        }
        assert_eq!(cache.len(), 10);

        cache.set_max_entries(5);
        assert_eq!(cache.len(), 5);
        assert!(cache.stat_evictions >= 5);
    }

    #[test]
    fn test_cache_hit_rate_zero_lookups() {
        let cache = QueryCache::new(10);
        assert_eq!(cache.hit_rate(), 0.0);
    }
}
