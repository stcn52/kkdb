// ── Bloom Filter ────────────────────────────────────────────────────────────
//
// Space-efficient probabilistic data structure for set membership testing.
// Used to quickly reject BTree lookups for keys that definitely don't exist.
//
// ## False Positive Rate
//
// For m bits, k hash functions, and n inserted items:
//   FPR ≈ (1 - e^(-kn/m))^k
//
// With default settings (1024 bytes = 8192 bits, 4 hash functions):
//   ~1000 items → ~5% FPR
//   ~500 items  → ~1.3% FPR
//
// ## Usage
//
// ```rust
// let mut bf = BloomFilter::new(1024, 4); // 1KB, 4 hashes
// bf.insert(b"key1");
// assert!(bf.may_contain(b"key1"));     // always true
// assert!(!bf.may_contain(b"unknown")); // probably false
// ```

/// Simple bloom filter using double-hashing (FNV-1a based).
#[derive(Debug, Clone)]
pub struct BloomFilter {
    /// Bit vector stored as bytes.
    bits: Vec<u8>,
    /// Number of hash functions.
    num_hashes: u32,
    /// Number of items inserted.
    count: u64,
}

impl BloomFilter {
    /// Create a new bloom filter with `size_bytes` of storage and `num_hashes` hash functions.
    pub fn new(size_bytes: usize, num_hashes: u32) -> Self {
        assert!(size_bytes > 0, "bloom filter size must be > 0");
        assert!(num_hashes > 0, "num_hashes must be > 0");
        Self {
            bits: vec![0u8; size_bytes],
            num_hashes: num_hashes.min(16), // cap at 16
            count: 0,
        }
    }

    /// Create a bloom filter optimized for `expected_items` with target FPR ~1%.
    pub fn for_capacity(expected_items: usize) -> Self {
        // Optimal m = -n*ln(p) / (ln2)^2, for p=0.01
        let n = expected_items.max(1) as f64;
        let m_bits = (-n * 0.01_f64.ln() / (2.0_f64.ln().powi(2))).ceil() as usize;
        let m_bytes = (m_bits / 8).max(8); // at least 8 bytes
        // Optimal k = (m/n) * ln(2)
        let k = ((m_bytes as f64 * 8.0 / n) * 2.0_f64.ln()).ceil() as u32;
        let k = k.clamp(1, 16);
        Self::new(m_bytes, k)
    }

    /// Insert a key into the bloom filter.
    pub fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = Self::hash_pair(key);
        let num_bits = self.bits.len() as u64 * 8;
        for i in 0..self.num_hashes {
            let bit_idx = (h1.wrapping_add(h2.wrapping_mul(i as u64))) % num_bits;
            let byte_idx = (bit_idx / 8) as usize;
            let bit_pos = (bit_idx % 8) as u8;
            self.bits[byte_idx] |= 1 << bit_pos;
        }
        self.count += 1;
    }

    /// Check if a key may be in the set. Returns false only if definitely not present.
    pub fn may_contain(&self, key: &[u8]) -> bool {
        let (h1, h2) = Self::hash_pair(key);
        let num_bits = self.bits.len() as u64 * 8;
        for i in 0..self.num_hashes {
            let bit_idx = (h1.wrapping_add(h2.wrapping_mul(i as u64))) % num_bits;
            let byte_idx = (bit_idx / 8) as usize;
            let bit_pos = (bit_idx % 8) as u8;
            if self.bits[byte_idx] & (1 << bit_pos) == 0 {
                return false;
            }
        }
        true
    }

    /// Number of items inserted.
    pub fn item_count(&self) -> u64 {
        self.count
    }

    /// Size of the bit vector in bytes.
    pub fn size_bytes(&self) -> usize {
        self.bits.len()
    }

    /// Estimated fill ratio (fraction of bits set to 1).
    pub fn fill_ratio(&self) -> f64 {
        let set_bits: u64 = self.bits.iter().map(|b| b.count_ones() as u64).sum();
        let total_bits = self.bits.len() as u64 * 8;
        set_bits as f64 / total_bits as f64
    }

    /// Estimated false positive rate based on current fill.
    pub fn estimated_fpr(&self) -> f64 {
        let ratio = self.fill_ratio();
        ratio.powi(self.num_hashes as i32)
    }

    /// Reset the bloom filter (clear all bits).
    pub fn clear(&mut self) {
        self.bits.fill(0);
        self.count = 0;
    }

    /// Merge another bloom filter into this one (OR operation).
    /// Both filters must have the same size and num_hashes.
    pub fn merge(&mut self, other: &BloomFilter) -> bool {
        if self.bits.len() != other.bits.len() || self.num_hashes != other.num_hashes {
            return false;
        }
        for (a, b) in self.bits.iter_mut().zip(other.bits.iter()) {
            *a |= *b;
        }
        self.count += other.count;
        true
    }

    /// Serialize the bloom filter to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.bits.len());
        out.extend_from_slice(&(self.bits.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.num_hashes.to_le_bytes());
        out.extend_from_slice(&(self.count as u32).to_le_bytes());
        out.extend_from_slice(&self.bits);
        out
    }

    /// Deserialize a bloom filter from bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        let size = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
        let num_hashes = u32::from_le_bytes(data[4..8].try_into().ok()?);
        let count = u32::from_le_bytes(data[8..12].try_into().ok()?) as u64;
        if data.len() < 12 + size {
            return None;
        }
        Some(Self {
            bits: data[12..12 + size].to_vec(),
            num_hashes,
            count,
        })
    }

    // ── Hash functions ──────────────────────────────────────────────────────

    /// Compute two independent hash values using FNV-1a variants.
    fn hash_pair(key: &[u8]) -> (u64, u64) {
        // FNV-1a hash
        let mut h1: u64 = 0xcbf29ce484222325;
        for &b in key {
            h1 ^= b as u64;
            h1 = h1.wrapping_mul(0x100000001b3);
        }

        // FNV-1a with different seed
        let mut h2: u64 = 0x6c62272e07bb0142;
        for &b in key {
            h2 ^= b as u64;
            h2 = h2.wrapping_mul(0x100000001b3);
        }

        (h1, h2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_insert_and_query() {
        let mut bf = BloomFilter::new(128, 4);
        bf.insert(b"hello");
        bf.insert(b"world");
        assert!(bf.may_contain(b"hello"));
        assert!(bf.may_contain(b"world"));
        assert_eq!(bf.item_count(), 2);
    }

    #[test]
    fn test_bloom_negative() {
        let bf = BloomFilter::new(1024, 4);
        // Empty filter should not contain anything
        assert!(!bf.may_contain(b"anything"));
    }

    #[test]
    fn test_bloom_false_positive_rate() {
        let mut bf = BloomFilter::new(1024, 4);
        // Insert 100 items
        for i in 0..100u64 {
            bf.insert(&i.to_le_bytes());
        }

        // Check 1000 items NOT inserted
        let mut false_positives = 0;
        for i in 1000..2000u64 {
            if bf.may_contain(&i.to_le_bytes()) {
                false_positives += 1;
            }
        }

        // FPR should be well below 10% for 100 items in 1KB
        assert!(
            false_positives < 100,
            "too many false positives: {}/1000",
            false_positives
        );
    }

    #[test]
    fn test_bloom_for_capacity() {
        let bf = BloomFilter::for_capacity(1000);
        assert!(bf.size_bytes() > 0);
        assert!(bf.item_count() == 0);
    }

    #[test]
    fn test_bloom_fill_ratio() {
        let mut bf = BloomFilter::new(64, 3);
        assert!((bf.fill_ratio() - 0.0).abs() < 1e-10);
        bf.insert(b"test");
        assert!(bf.fill_ratio() > 0.0);
    }

    #[test]
    fn test_bloom_clear() {
        let mut bf = BloomFilter::new(64, 3);
        bf.insert(b"x");
        assert!(bf.may_contain(b"x"));
        bf.clear();
        assert!(!bf.may_contain(b"x"));
        assert_eq!(bf.item_count(), 0);
    }

    #[test]
    fn test_bloom_merge() {
        let mut bf1 = BloomFilter::new(64, 3);
        let mut bf2 = BloomFilter::new(64, 3);
        bf1.insert(b"a");
        bf2.insert(b"b");
        assert!(bf1.merge(&bf2));
        assert!(bf1.may_contain(b"a"));
        assert!(bf1.may_contain(b"b"));
    }

    #[test]
    fn test_bloom_merge_incompatible() {
        let mut bf1 = BloomFilter::new(64, 3);
        let bf2 = BloomFilter::new(128, 3); // different size
        assert!(!bf1.merge(&bf2));
    }

    #[test]
    fn test_bloom_serialize_roundtrip() {
        let mut bf = BloomFilter::new(256, 5);
        bf.insert(b"serialize");
        bf.insert(b"roundtrip");

        let bytes = bf.to_bytes();
        let bf2 = BloomFilter::from_bytes(&bytes).unwrap();
        assert!(bf2.may_contain(b"serialize"));
        assert!(bf2.may_contain(b"roundtrip"));
        assert_eq!(bf2.item_count(), 2);
        assert_eq!(bf2.size_bytes(), 256);
    }

    #[test]
    fn test_bloom_from_bytes_invalid() {
        assert!(BloomFilter::from_bytes(&[]).is_none());
        assert!(BloomFilter::from_bytes(&[0u8; 11]).is_none());
        // Valid header but insufficient data
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(&100u32.to_le_bytes()); // size=100
        assert!(BloomFilter::from_bytes(&data).is_none()); // need 112 bytes
    }

    #[test]
    fn test_bloom_estimated_fpr() {
        let mut bf = BloomFilter::new(1024, 4);
        assert!((bf.estimated_fpr() - 0.0).abs() < 1e-10);
        for i in 0..100u64 {
            bf.insert(&i.to_le_bytes());
        }
        let fpr = bf.estimated_fpr();
        assert!(fpr > 0.0);
        assert!(fpr < 1.0);
    }
}
