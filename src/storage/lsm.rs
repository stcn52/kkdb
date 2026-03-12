// R12 – LSM-Tree compaction simulator + dictionary compression + cold/hot tiering.
//
// Provides:
//   - `LsmLevel`: represents one level of an LSM-Tree
//   - `LsmCompactor`: simulates leveled compaction strategy
//   - `DictionaryCompressor`: maps repeated values → short integer codes
//   - `HotColdTiering`: tracks access frequency to classify data as hot or cold

use std::collections::HashMap;

// ── LSM-Tree Compaction ───────────────────────────────────────────────

/// Represents a single level (L0, L1, …) of an LSM-Tree.
#[derive(Debug, Clone)]
pub struct LsmLevel {
    /// Level number (0 = memtable flush target, 1+ = sorted runs).
    pub level: usize,
    /// Number of sorted runs (SSTables) at this level.
    pub run_count: usize,
    /// Maximum number of runs before compaction is triggered.
    pub max_runs: usize,
    /// Total data size in bytes at this level.
    pub size_bytes: u64,
    /// Size amplification factor to the next level.
    pub size_ratio: usize,
}

impl LsmLevel {
    pub fn new(level: usize, max_runs: usize, size_ratio: usize) -> Self {
        Self {
            level,
            run_count: 0,
            max_runs,
            size_bytes: 0,
            size_ratio,
        }
    }

    /// Check if this level needs compaction.
    pub fn needs_compaction(&self) -> bool {
        self.run_count >= self.max_runs
    }

    /// Add a new run (e.g., flushed memtable or compacted output).
    pub fn add_run(&mut self, size_bytes: u64) {
        self.run_count += 1;
        self.size_bytes += size_bytes;
    }

    /// Remove all runs (after compaction merges them).
    pub fn clear_runs(&mut self) {
        self.run_count = 0;
        self.size_bytes = 0;
    }
}

/// Simulates leveled compaction for an LSM-Tree.
pub struct LsmCompactor {
    levels: Vec<LsmLevel>,
    total_compactions: usize,
    total_bytes_compacted: u64,
}

impl LsmCompactor {
    /// Create a compactor with `num_levels` levels.
    ///
    /// - L0 allows `l0_max_runs` runs before compacting into L1.
    /// - Each subsequent level has capacity = prior_capacity × `size_ratio`.
    pub fn new(num_levels: usize, l0_max_runs: usize, size_ratio: usize) -> Self {
        let mut levels = Vec::with_capacity(num_levels);
        for i in 0..num_levels {
            let max_runs = if i == 0 { l0_max_runs } else { size_ratio };
            levels.push(LsmLevel::new(i, max_runs, size_ratio));
        }
        Self {
            levels,
            total_compactions: 0,
            total_bytes_compacted: 0,
        }
    }

    /// Flush a memtable to L0.
    pub fn flush_memtable(&mut self, size_bytes: u64) {
        if !self.levels.is_empty() {
            self.levels[0].add_run(size_bytes);
        }
    }

    /// Run one round of compaction: check each level bottom-up and compact if needed.
    ///
    /// Returns the number of compactions performed.
    pub fn compact(&mut self) -> usize {
        let mut compactions = 0;
        let num_levels = self.levels.len();

        for i in 0..num_levels.saturating_sub(1) {
            if self.levels[i].needs_compaction() {
                let merged_size = self.levels[i].size_bytes;
                self.total_bytes_compacted += merged_size;
                self.levels[i].clear_runs();
                self.levels[i + 1].add_run(merged_size);
                compactions += 1;
            }
        }

        self.total_compactions += compactions;
        compactions
    }

    /// Compact all levels until nothing needs compaction.
    pub fn compact_all(&mut self) -> usize {
        let mut total = 0;
        loop {
            let n = self.compact();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }

    /// Get level info.
    pub fn level(&self, idx: usize) -> Option<&LsmLevel> {
        self.levels.get(idx)
    }

    /// Number of levels.
    pub fn num_levels(&self) -> usize {
        self.levels.len()
    }

    /// Total compactions performed.
    pub fn total_compactions(&self) -> usize {
        self.total_compactions
    }

    /// Total bytes compacted.
    pub fn total_bytes_compacted(&self) -> u64 {
        self.total_bytes_compacted
    }

    /// Check if any level needs compaction.
    pub fn needs_compaction(&self) -> bool {
        self.levels.iter().any(|l| l.needs_compaction())
    }
}

// ── Dictionary Compression ───────────────────────────────────────────

/// Bidirectional dictionary for encoding repeated string values as integers.
///
/// Commonly used for compressing columns with low cardinality.
pub struct DictionaryCompressor {
    /// Value → code.
    forward: HashMap<String, u32>,
    /// Code → value.
    reverse: Vec<String>,
}

impl Default for DictionaryCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl DictionaryCompressor {
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: Vec::new(),
        }
    }

    /// Encode a value. Returns the code (existing or newly assigned).
    pub fn encode(&mut self, value: &str) -> u32 {
        if let Some(&code) = self.forward.get(value) {
            return code;
        }
        let code = self.reverse.len() as u32;
        self.forward.insert(value.to_string(), code);
        self.reverse.push(value.to_string());
        code
    }

    /// Decode a code back to the original value.
    pub fn decode(&self, code: u32) -> Option<&str> {
        self.reverse.get(code as usize).map(|s| s.as_str())
    }

    /// Number of distinct values in the dictionary.
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    /// Whether the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Check if a value is already in the dictionary.
    pub fn contains(&self, value: &str) -> bool {
        self.forward.contains_key(value)
    }

    /// Estimate memory savings for `n` occurrences of average `avg_len`-byte values.
    ///
    /// Dictionary stores each value once + 4-byte codes.
    /// Without dictionary: n × avg_len bytes.
    /// With dictionary: dict_entries × avg_len + n × 4 bytes.
    pub fn estimate_savings(&self, total_occurrences: usize, avg_len: usize) -> i64 {
        let without = (total_occurrences * avg_len) as i64;
        let with = (self.len() * avg_len + total_occurrences * 4) as i64;
        without - with
    }

    /// Clear the dictionary.
    pub fn clear(&mut self) {
        self.forward.clear();
        self.reverse.clear();
    }
}

// ── Hot/Cold Data Tiering ─────────────────────────────────────────────

/// Tracks access frequency for pages to classify as hot or cold.
pub struct HotColdTiering {
    /// Page ID → access count.
    access_counts: HashMap<u32, u64>,
    /// Threshold: pages with access_count ≥ threshold are "hot".
    hot_threshold: u64,
    /// Total accesses tracked.
    total_accesses: u64,
}

impl HotColdTiering {
    pub fn new(hot_threshold: u64) -> Self {
        Self {
            access_counts: HashMap::new(),
            hot_threshold,
            total_accesses: 0,
        }
    }

    /// Record an access to a page.
    pub fn record_access(&mut self, page_id: u32) {
        *self.access_counts.entry(page_id).or_insert(0) += 1;
        self.total_accesses += 1;
    }

    /// Check if a page is "hot" (frequently accessed).
    pub fn is_hot(&self, page_id: u32) -> bool {
        self.access_counts.get(&page_id).copied().unwrap_or(0) >= self.hot_threshold
    }

    /// Check if a page is "cold" (infrequently accessed).
    pub fn is_cold(&self, page_id: u32) -> bool {
        !self.is_hot(page_id)
    }

    /// Get all hot page IDs.
    pub fn hot_pages(&self) -> Vec<u32> {
        self.access_counts
            .iter()
            .filter(|(_, &count)| count >= self.hot_threshold)
            .map(|(&id, _)| id)
            .collect()
    }

    /// Get all cold page IDs.
    pub fn cold_pages(&self) -> Vec<u32> {
        self.access_counts
            .iter()
            .filter(|(_, &count)| count < self.hot_threshold)
            .map(|(&id, _)| id)
            .collect()
    }

    /// Get the access count for a page.
    pub fn access_count(&self, page_id: u32) -> u64 {
        self.access_counts.get(&page_id).copied().unwrap_or(0)
    }

    /// Number of tracked pages.
    pub fn tracked_pages(&self) -> usize {
        self.access_counts.len()
    }

    /// Total accesses recorded.
    pub fn total_accesses(&self) -> u64 {
        self.total_accesses
    }

    /// Decay all access counts by dividing by 2 (aging).
    ///
    /// This allows recently accessed pages to stay hot while old accesses fade.
    pub fn decay(&mut self) {
        for count in self.access_counts.values_mut() {
            *count /= 2;
        }
        // Remove pages that decayed to zero
        self.access_counts.retain(|_, c| *c > 0);
    }

    /// Reset all tracking data.
    pub fn reset(&mut self) {
        self.access_counts.clear();
        self.total_accesses = 0;
    }

    /// Set the hot threshold.
    pub fn set_hot_threshold(&mut self, threshold: u64) {
        self.hot_threshold = threshold;
    }

    /// Current hot threshold.
    pub fn hot_threshold(&self) -> u64 {
        self.hot_threshold
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // LSM Compaction tests
    #[test]
    fn lsm_flush_and_compact() {
        let mut c = LsmCompactor::new(3, 4, 10);
        for _ in 0..4 {
            c.flush_memtable(1000);
        }
        assert!(c.needs_compaction());
        let n = c.compact();
        assert_eq!(n, 1);
        assert_eq!(c.level(0).unwrap().run_count, 0);
        assert_eq!(c.level(1).unwrap().run_count, 1);
    }

    #[test]
    fn lsm_cascading_compaction() {
        let mut c = LsmCompactor::new(4, 2, 2);
        // Fill L0 twice, then compact cascading through levels
        for _ in 0..8 {
            c.flush_memtable(100);
            c.compact_all();
        }
        assert!(c.total_compactions() > 0);
        assert!(!c.needs_compaction());
    }

    #[test]
    fn lsm_level_info() {
        let c = LsmCompactor::new(3, 4, 10);
        assert_eq!(c.num_levels(), 3);
        let l0 = c.level(0).unwrap();
        assert_eq!(l0.max_runs, 4);
        assert_eq!(l0.run_count, 0);
    }

    // Dictionary Compression tests
    #[test]
    fn dict_encode_decode() {
        let mut d = DictionaryCompressor::new();
        let c1 = d.encode("apple");
        let c2 = d.encode("banana");
        let c3 = d.encode("apple"); // same code
        assert_eq!(c1, c3);
        assert_ne!(c1, c2);
        assert_eq!(d.decode(c1), Some("apple"));
        assert_eq!(d.decode(c2), Some("banana"));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn dict_contains() {
        let mut d = DictionaryCompressor::new();
        d.encode("hello");
        assert!(d.contains("hello"));
        assert!(!d.contains("world"));
    }

    #[test]
    fn dict_savings() {
        let mut d = DictionaryCompressor::new();
        d.encode("category_a");
        d.encode("category_b");
        // 1000 occurrences of avg 10-byte values
        let savings = d.estimate_savings(1000, 10);
        // Without: 10000 bytes. With: 20 (dict) + 4000 (codes) = 4020. Savings = 5980.
        assert!(savings > 0);
    }

    #[test]
    fn dict_clear() {
        let mut d = DictionaryCompressor::new();
        d.encode("x");
        d.clear();
        assert!(d.is_empty());
        assert_eq!(d.decode(0), None);
    }

    // Hot/Cold Tiering tests
    #[test]
    fn tiering_hot_cold() {
        let mut t = HotColdTiering::new(5);
        for _ in 0..10 {
            t.record_access(1);
        }
        for _ in 0..2 {
            t.record_access(2);
        }
        assert!(t.is_hot(1));
        assert!(t.is_cold(2));
        assert!(t.is_cold(99)); // never accessed
    }

    #[test]
    fn tiering_hot_cold_pages() {
        let mut t = HotColdTiering::new(3);
        for _ in 0..5 {
            t.record_access(10);
        }
        for _ in 0..1 {
            t.record_access(20);
        }
        for _ in 0..4 {
            t.record_access(30);
        }
        let hot = t.hot_pages();
        assert!(hot.contains(&10));
        assert!(hot.contains(&30));
        assert!(!hot.contains(&20));
    }

    #[test]
    fn tiering_decay() {
        let mut t = HotColdTiering::new(5);
        for _ in 0..10 {
            t.record_access(1);
        }
        t.record_access(2); // count=1
        assert!(t.is_hot(1)); // count=10
        t.decay();
        assert!(t.is_hot(1)); // count=5, threshold=5 → still hot
        t.decay();
        assert!(t.is_cold(1)); // count=2, < 5
                               // Page 2 decayed to 0 and removed
        assert_eq!(t.tracked_pages(), 1);
    }

    #[test]
    fn tiering_reset() {
        let mut t = HotColdTiering::new(5);
        t.record_access(1);
        t.reset();
        assert_eq!(t.tracked_pages(), 0);
        assert_eq!(t.total_accesses(), 0);
    }
}
