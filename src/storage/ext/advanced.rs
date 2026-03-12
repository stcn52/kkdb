// R15 – Storage engine deep enhancement: write amplification tracking,
//       layered bloom filters, partition pruning, page verification chain.
//
// Provides:
//   - `WriteAmpTracker`: measures and reports write amplification factor
//   - `LayeredBloomFilter`: multi-level bloom filter for LSM-style lookups
//   - `PartitionPruner`: eliminates unnecessary partitions during scans
//   - `PageVerificationChain`: chained page checksums for integrity

use std::collections::HashMap;

// ── Write Amplification Tracker ───────────────────────────────────────

/// Tracks write amplification (ratio of physical writes to logical writes).
pub struct WriteAmpTracker {
    logical_bytes: u64,
    physical_bytes: u64,
    /// Per-level write accounting (for LSM compaction).
    level_writes: Vec<u64>,
}

impl WriteAmpTracker {
    pub fn new(num_levels: usize) -> Self {
        Self {
            logical_bytes: 0,
            physical_bytes: 0,
            level_writes: vec![0; num_levels],
        }
    }

    /// Record a logical write (user-initiated).
    pub fn record_logical(&mut self, bytes: u64) {
        self.logical_bytes += bytes;
    }

    /// Record a physical write (actual I/O).
    pub fn record_physical(&mut self, bytes: u64, level: Option<usize>) {
        self.physical_bytes += bytes;
        if let Some(l) = level {
            if l < self.level_writes.len() {
                self.level_writes[l] += bytes;
            }
        }
    }

    /// Write amplification factor (WAF).
    pub fn waf(&self) -> f64 {
        if self.logical_bytes == 0 {
            return 1.0;
        }
        self.physical_bytes as f64 / self.logical_bytes as f64
    }

    /// Per-level breakdown.
    pub fn level_breakdown(&self) -> &[u64] {
        &self.level_writes
    }

    pub fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub fn physical_bytes(&self) -> u64 {
        self.physical_bytes
    }

    pub fn reset(&mut self) {
        self.logical_bytes = 0;
        self.physical_bytes = 0;
        for l in &mut self.level_writes {
            *l = 0;
        }
    }
}

// ── Layered Bloom Filter ──────────────────────────────────────────────

/// A single bloom filter layer (bit array + hash count).
#[derive(Debug, Clone)]
pub struct BloomLayer {
    bits: Vec<bool>,
    num_hashes: usize,
    item_count: usize,
}

impl BloomLayer {
    pub fn new(capacity: usize, num_hashes: usize) -> Self {
        let bits_size = capacity * 10; // 10 bits per item
        Self {
            bits: vec![false; bits_size.max(64)],
            num_hashes,
            item_count: 0,
        }
    }

    fn hash(&self, key: &[u8], seed: usize) -> usize {
        let mut h: u64 = seed as u64;
        for &b in key {
            h = h.wrapping_mul(31).wrapping_add(b as u64);
        }
        h as usize % self.bits.len()
    }

    pub fn insert(&mut self, key: &[u8]) {
        for i in 0..self.num_hashes {
            let idx = self.hash(key, i + 1);
            self.bits[idx] = true;
        }
        self.item_count += 1;
    }

    pub fn may_contain(&self, key: &[u8]) -> bool {
        for i in 0..self.num_hashes {
            let idx = self.hash(key, i + 1);
            if !self.bits[idx] {
                return false;
            }
        }
        true
    }

    pub fn item_count(&self) -> usize {
        self.item_count
    }

    /// Estimated false positive rate.
    pub fn false_positive_rate(&self) -> f64 {
        let m = self.bits.len() as f64;
        let k = self.num_hashes as f64;
        let n = self.item_count as f64;
        (1.0 - (-(k * n) / m).exp()).powf(k)
    }
}

/// Multi-level bloom filter for LSM-style lookups.
pub struct LayeredBloomFilter {
    layers: Vec<BloomLayer>,
}

impl LayeredBloomFilter {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Add a new layer.
    pub fn add_layer(&mut self, capacity: usize, num_hashes: usize) {
        self.layers.push(BloomLayer::new(capacity, num_hashes));
    }

    /// Insert into a specific layer.
    pub fn insert(&mut self, layer: usize, key: &[u8]) {
        if layer < self.layers.len() {
            self.layers[layer].insert(key);
        }
    }

    /// Check all layers: returns the first layer that may contain the key, or None.
    pub fn lookup(&self, key: &[u8]) -> Option<usize> {
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.may_contain(key) {
                return Some(i);
            }
        }
        None
    }

    /// Check if key might exist in any layer.
    pub fn may_contain(&self, key: &[u8]) -> bool {
        self.layers.iter().any(|l| l.may_contain(key))
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    pub fn total_items(&self) -> usize {
        self.layers.iter().map(|l| l.item_count()).sum()
    }
}

// ── Partition Pruner ──────────────────────────────────────────────────

/// Partition boundary definition.
#[derive(Debug, Clone)]
pub struct PartitionDef {
    pub partition_id: u32,
    pub name: String,
    /// Lower bound (inclusive). None = unbounded below.
    pub lower: Option<i64>,
    /// Upper bound (exclusive). None = unbounded above.
    pub upper: Option<i64>,
}

impl PartitionDef {
    pub fn new(id: u32, name: &str, lower: Option<i64>, upper: Option<i64>) -> Self {
        Self {
            partition_id: id,
            name: name.to_string(),
            lower,
            upper,
        }
    }

    /// Check if a value falls within this partition.
    pub fn contains(&self, value: i64) -> bool {
        let above_lower = match self.lower {
            Some(lo) => value >= lo,
            None => true,
        };
        let below_upper = match self.upper {
            Some(hi) => value < hi,
            None => true,
        };
        above_lower && below_upper
    }

    /// Check if a range [lo, hi] overlaps this partition.
    pub fn overlaps_range(&self, lo: i64, hi: i64) -> bool {
        let range_end_after_lower = match self.lower {
            Some(l) => hi >= l,
            None => true,
        };
        let range_start_before_upper = match self.upper {
            Some(u) => lo < u,
            None => true,
        };
        range_end_after_lower && range_start_before_upper
    }
}

/// Prunes partitions based on query predicates.
pub struct PartitionPruner {
    partitions: Vec<PartitionDef>,
}

impl PartitionPruner {
    pub fn new(partitions: Vec<PartitionDef>) -> Self {
        Self { partitions }
    }

    /// Prune to partitions containing a specific value.
    pub fn prune_eq(&self, value: i64) -> Vec<u32> {
        self.partitions
            .iter()
            .filter(|p| p.contains(value))
            .map(|p| p.partition_id)
            .collect()
    }

    /// Prune to partitions overlapping a range [lo, hi].
    pub fn prune_range(&self, lo: i64, hi: i64) -> Vec<u32> {
        self.partitions
            .iter()
            .filter(|p| p.overlaps_range(lo, hi))
            .map(|p| p.partition_id)
            .collect()
    }

    /// Prune to partitions for an IN list of values.
    pub fn prune_in(&self, values: &[i64]) -> Vec<u32> {
        let mut result: Vec<u32> = Vec::new();
        for p in &self.partitions {
            if values.iter().any(|&v| p.contains(v)) {
                if !result.contains(&p.partition_id) {
                    result.push(p.partition_id);
                }
            }
        }
        result
    }

    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }
}

// ── Page Verification Chain ───────────────────────────────────────────

/// Chained page checksums — each page includes the previous page's checksum.
pub struct PageVerificationChain {
    /// page_id → (checksum, prev_page_checksum)
    checksums: HashMap<u32, (u32, u32)>,
    last_page_id: Option<u32>,
    last_checksum: u32,
}

impl PageVerificationChain {
    pub fn new() -> Self {
        Self {
            checksums: HashMap::new(),
            last_page_id: None,
            last_checksum: 0,
        }
    }

    fn compute_checksum(data: &[u8], chain_val: u32) -> u32 {
        let mut h: u32 = chain_val;
        for &b in data {
            h = h.wrapping_mul(31).wrapping_add(b as u32);
        }
        h
    }

    /// Append a page to the verification chain.
    pub fn append(&mut self, page_id: u32, data: &[u8]) -> u32 {
        let checksum = Self::compute_checksum(data, self.last_checksum);
        self.checksums
            .insert(page_id, (checksum, self.last_checksum));
        self.last_page_id = Some(page_id);
        self.last_checksum = checksum;
        checksum
    }

    /// Verify a page's checksum.
    pub fn verify(&self, page_id: u32, data: &[u8]) -> bool {
        if let Some(&(expected, prev_cs)) = self.checksums.get(&page_id) {
            Self::compute_checksum(data, prev_cs) == expected
        } else {
            false
        }
    }

    /// Get the chain head checksum.
    pub fn head_checksum(&self) -> u32 {
        self.last_checksum
    }

    /// Verify the full chain integrity (sequential pages must chain correctly).
    pub fn verify_chain(&self, pages: &[(u32, &[u8])]) -> bool {
        let mut prev_cs = 0u32;
        for &(page_id, data) in pages {
            if let Some(&(expected, expected_prev)) = self.checksums.get(&page_id) {
                if expected_prev != prev_cs {
                    return false;
                }
                let actual = Self::compute_checksum(data, prev_cs);
                if actual != expected {
                    return false;
                }
                prev_cs = expected;
            } else {
                return false;
            }
        }
        true
    }

    pub fn page_count(&self) -> usize {
        self.checksums.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_amp_tracker() {
        let mut wat = WriteAmpTracker::new(3);
        wat.record_logical(1000);
        wat.record_physical(1000, Some(0));
        wat.record_physical(2000, Some(1));
        assert!((wat.waf() - 3.0).abs() < 0.01); // 3000/1000
        assert_eq!(wat.level_breakdown()[0], 1000);
        assert_eq!(wat.level_breakdown()[1], 2000);
    }

    #[test]
    fn write_amp_reset() {
        let mut wat = WriteAmpTracker::new(2);
        wat.record_logical(100);
        wat.record_physical(200, None);
        wat.reset();
        assert_eq!(wat.logical_bytes(), 0);
        assert_eq!(wat.physical_bytes(), 0);
    }

    #[test]
    fn bloom_layer_basic() {
        let mut bl = BloomLayer::new(100, 3);
        bl.insert(b"hello");
        bl.insert(b"world");
        assert!(bl.may_contain(b"hello"));
        assert!(bl.may_contain(b"world"));
        assert!(!bl.may_contain(b"missing"));
        assert_eq!(bl.item_count(), 2);
    }

    #[test]
    fn layered_bloom_filter() {
        let mut lbf = LayeredBloomFilter::new();
        lbf.add_layer(100, 3);
        lbf.add_layer(200, 3);
        lbf.insert(0, b"key1");
        lbf.insert(1, b"key2");
        assert_eq!(lbf.lookup(b"key1"), Some(0));
        assert_eq!(lbf.lookup(b"key2"), Some(1));
        assert!(lbf.may_contain(b"key1"));
        assert_eq!(lbf.total_items(), 2);
    }

    #[test]
    fn partition_pruner_eq() {
        let pruner = PartitionPruner::new(vec![
            PartitionDef::new(0, "p0", None, Some(100)),
            PartitionDef::new(1, "p1", Some(100), Some(200)),
            PartitionDef::new(2, "p2", Some(200), None),
        ]);
        assert_eq!(pruner.prune_eq(50), vec![0]);
        assert_eq!(pruner.prune_eq(150), vec![1]);
        assert_eq!(pruner.prune_eq(300), vec![2]);
    }

    #[test]
    fn partition_pruner_range() {
        let pruner = PartitionPruner::new(vec![
            PartitionDef::new(0, "p0", None, Some(100)),
            PartitionDef::new(1, "p1", Some(100), Some(200)),
            PartitionDef::new(2, "p2", Some(200), None),
        ]);
        let result = pruner.prune_range(50, 150);
        assert_eq!(result, vec![0, 1]); // overlaps p0 and p1
    }

    #[test]
    fn partition_pruner_in() {
        let pruner = PartitionPruner::new(vec![
            PartitionDef::new(0, "p0", None, Some(100)),
            PartitionDef::new(1, "p1", Some(100), Some(200)),
            PartitionDef::new(2, "p2", Some(200), None),
        ]);
        let result = pruner.prune_in(&[50, 250]);
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn page_verification_chain_basic() {
        let mut chain = PageVerificationChain::new();
        let cs1 = chain.append(1, b"page1data");
        let cs2 = chain.append(2, b"page2data");
        assert_ne!(cs1, cs2);
        assert!(chain.verify(1, b"page1data"));
        assert!(chain.verify(2, b"page2data"));
        assert!(!chain.verify(1, b"tampered"));
    }

    #[test]
    fn page_verification_chain_full() {
        let mut chain = PageVerificationChain::new();
        chain.append(1, b"aaa");
        chain.append(2, b"bbb");
        chain.append(3, b"ccc");
        assert!(chain.verify_chain(&[
            (1, b"aaa" as &[u8]),
            (2, b"bbb" as &[u8]),
            (3, b"ccc" as &[u8]),
        ]));
    }

    #[test]
    fn bloom_false_positive_rate() {
        let mut bl = BloomLayer::new(1000, 7);
        for i in 0..500 {
            bl.insert(format!("key{}", i).as_bytes());
        }
        let fpr = bl.false_positive_rate();
        assert!(fpr < 0.05); // should be low for items < capacity
    }
}
