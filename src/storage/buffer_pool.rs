// R14 – Storage engine advanced optimization: adaptive buffer pool,
//       LRU-K page eviction, read-ahead prefetch, write coalescing.
//
// Provides:
//   - `LruKEntry` + `LruKEvictor`: LRU-K(2) page eviction policy
//   - `ReadAheadManager`: sequential and random prefetch scheduling
//   - `WriteCoalescer`: batches dirty page flushes
//   - `AdaptiveBufferPool`: combines all components

use std::collections::HashMap;
use std::time::Instant;

// ── LRU-K Eviction ───────────────────────────────────────────────────

/// History of accesses for a single page (LRU-K, K=2).
#[derive(Debug, Clone)]
pub struct LruKEntry {
    pub page_id: u32,
    /// Timestamps of the last K accesses (most recent last).
    pub history: Vec<Instant>,
    pub dirty: bool,
    pub pinned: bool,
    pub k: usize,
}

impl LruKEntry {
    pub fn new(page_id: u32, k: usize) -> Self {
        Self {
            page_id,
            history: Vec::with_capacity(k),
            dirty: false,
            pinned: false,
            k,
        }
    }

    /// Record an access.
    pub fn access(&mut self) {
        let now = Instant::now();
        self.history.push(now);
        if self.history.len() > self.k {
            self.history.remove(0);
        }
    }

    /// Backward K-distance: time since the K-th most recent access.
    /// Larger distance = better eviction candidate.
    /// If fewer than K accesses, return u64::MAX (highest priority for eviction).
    pub fn backward_k_distance(&self) -> u64 {
        if self.history.len() < self.k {
            return u64::MAX;
        }
        self.history[0].elapsed().as_micros() as u64
    }
}

/// LRU-K(2) page eviction manager.
pub struct LruKEvictor {
    entries: HashMap<u32, LruKEntry>,
    k: usize,
    capacity: usize,
}

impl LruKEvictor {
    pub fn new(capacity: usize, k: usize) -> Self {
        Self {
            entries: HashMap::new(),
            k,
            capacity,
        }
    }

    /// Record page access. Returns true if the page was newly inserted.
    pub fn access(&mut self, page_id: u32) -> bool {
        let is_new = !self.entries.contains_key(&page_id);
        let entry = self
            .entries
            .entry(page_id)
            .or_insert_with(|| LruKEntry::new(page_id, self.k));
        entry.access();
        is_new
    }

    /// Mark a page as dirty.
    pub fn mark_dirty(&mut self, page_id: u32) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    /// Pin a page (prevent eviction).
    pub fn pin(&mut self, page_id: u32) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pinned = true;
        }
    }

    /// Unpin a page.
    pub fn unpin(&mut self, page_id: u32) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pinned = false;
        }
    }

    /// Select the best victim for eviction (largest backward K-distance, not pinned).
    pub fn select_victim(&self) -> Option<u32> {
        self.entries
            .values()
            .filter(|e| !e.pinned)
            .max_by_key(|e| e.backward_k_distance())
            .map(|e| e.page_id)
    }

    /// Evict a page.
    pub fn evict(&mut self, page_id: u32) -> Option<LruKEntry> {
        self.entries.remove(&page_id)
    }

    /// Number of pages tracked.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no tracked pages.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        self.entries.len() >= self.capacity
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Check if a specific page is tracked.
    pub fn contains(&self, page_id: u32) -> bool {
        self.entries.contains_key(&page_id)
    }

    /// Get dirty page IDs.
    pub fn dirty_pages(&self) -> Vec<u32> {
        self.entries
            .values()
            .filter(|e| e.dirty)
            .map(|e| e.page_id)
            .collect()
    }
}

// ── Read-Ahead Manager ────────────────────────────────────────────────

/// Read-ahead / prefetch strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchStrategy {
    None,
    Sequential,  // prefetch next N pages
    StrideBased, // detect stride pattern
}

/// Manages read-ahead prefetch scheduling.
pub struct ReadAheadManager {
    strategy: PrefetchStrategy,
    window_size: usize,
    /// History of recently accessed page IDs for stride detection.
    access_history: Vec<u32>,
    history_limit: usize,
}

impl ReadAheadManager {
    pub fn new(strategy: PrefetchStrategy, window_size: usize) -> Self {
        Self {
            strategy,
            window_size,
            access_history: Vec::new(),
            history_limit: 16,
        }
    }

    pub fn strategy(&self) -> PrefetchStrategy {
        self.strategy
    }

    pub fn set_strategy(&mut self, strategy: PrefetchStrategy) {
        self.strategy = strategy;
    }

    /// Record a page access and return pages to prefetch.
    pub fn on_access(&mut self, page_id: u32) -> Vec<u32> {
        self.access_history.push(page_id);
        if self.access_history.len() > self.history_limit {
            self.access_history.remove(0);
        }

        match self.strategy {
            PrefetchStrategy::None => Vec::new(),
            PrefetchStrategy::Sequential => {
                (1..=self.window_size as u32).map(|i| page_id + i).collect()
            }
            PrefetchStrategy::StrideBased => {
                if let Some(stride) = self.detect_stride() {
                    (1..=self.window_size as u32)
                        .map(|i| (page_id as i64 + stride * i as i64) as u32)
                        .collect()
                } else {
                    // Fallback to sequential
                    (1..=self.window_size as u32).map(|i| page_id + i).collect()
                }
            }
        }
    }

    /// Detect stride pattern from access history.
    fn detect_stride(&self) -> Option<i64> {
        if self.access_history.len() < 3 {
            return None;
        }
        let n = self.access_history.len();
        let d1 = self.access_history[n - 1] as i64 - self.access_history[n - 2] as i64;
        let d2 = self.access_history[n - 2] as i64 - self.access_history[n - 3] as i64;
        if d1 == d2 && d1 != 0 {
            Some(d1)
        } else {
            None
        }
    }

    pub fn window_size(&self) -> usize {
        self.window_size
    }
}

// ── Write Coalescer ───────────────────────────────────────────────────

/// Batches dirty page flushes to reduce I/O operations.
pub struct WriteCoalescer {
    /// Pending writes: page_id → data.
    pending: HashMap<u32, Vec<u8>>,
    /// Max number of pending writes before forced flush.
    max_pending: usize,
    /// Total bytes flushed.
    bytes_flushed: u64,
    /// Total flush operations.
    flush_count: u64,
}

impl WriteCoalescer {
    pub fn new(max_pending: usize) -> Self {
        Self {
            pending: HashMap::new(),
            max_pending,
            bytes_flushed: 0,
            flush_count: 0,
        }
    }

    /// Add a dirty page to the write batch. Returns true if flushing is needed.
    pub fn add_write(&mut self, page_id: u32, data: Vec<u8>) -> bool {
        self.pending.insert(page_id, data);
        self.pending.len() >= self.max_pending
    }

    /// Flush all pending writes. Returns (page_ids, total_bytes).
    pub fn flush(&mut self) -> (Vec<u32>, usize) {
        let page_ids: Vec<u32> = self.pending.keys().copied().collect();
        let total_bytes: usize = self.pending.values().map(|d| d.len()).sum();
        self.bytes_flushed += total_bytes as u64;
        self.flush_count += 1;
        self.pending.clear();
        (page_ids, total_bytes)
    }

    /// Number of pending writes.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn bytes_flushed(&self) -> u64 {
        self.bytes_flushed
    }

    pub fn flush_count(&self) -> u64 {
        self.flush_count
    }

    /// Check if a page has a pending write.
    pub fn has_pending(&self, page_id: u32) -> bool {
        self.pending.contains_key(&page_id)
    }
}

// ── Adaptive Buffer Pool ──────────────────────────────────────────────

/// Combines LRU-K eviction, read-ahead, and write coalescing.
pub struct AdaptiveBufferPool {
    pub evictor: LruKEvictor,
    pub read_ahead: ReadAheadManager,
    pub coalescer: WriteCoalescer,
    hit_count: u64,
    miss_count: u64,
}

impl AdaptiveBufferPool {
    pub fn new(capacity: usize, prefetch_window: usize, max_pending_writes: usize) -> Self {
        Self {
            evictor: LruKEvictor::new(capacity, 2),
            read_ahead: ReadAheadManager::new(PrefetchStrategy::Sequential, prefetch_window),
            coalescer: WriteCoalescer::new(max_pending_writes),
            hit_count: 0,
            miss_count: 0,
        }
    }

    /// Access a page. Returns prefetch suggestions if any.
    pub fn access_page(&mut self, page_id: u32) -> Vec<u32> {
        let is_new = self.evictor.access(page_id);
        if is_new {
            self.miss_count += 1;
        } else {
            self.hit_count += 1;
        }
        self.read_ahead.on_access(page_id)
    }

    /// Write a dirty page.
    pub fn write_page(&mut self, page_id: u32, data: Vec<u8>) -> bool {
        self.evictor.mark_dirty(page_id);
        self.coalescer.add_write(page_id, data)
    }

    /// Hit ratio (0.0 – 1.0).
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hit_count + self.miss_count;
        if total == 0 {
            return 0.0;
        }
        self.hit_count as f64 / total as f64
    }

    pub fn hit_count(&self) -> u64 {
        self.hit_count
    }

    pub fn miss_count(&self) -> u64 {
        self.miss_count
    }

    /// Evict enough pages to make room for `count` new pages.
    pub fn evict_pages(&mut self, count: usize) -> Vec<u32> {
        let mut evicted = Vec::new();
        for _ in 0..count {
            if let Some(victim) = self.evictor.select_victim() {
                self.evictor.evict(victim);
                evicted.push(victim);
            } else {
                break;
            }
        }
        evicted
    }

    /// Adapt prefetch strategy based on hit ratio.
    pub fn adapt(&mut self) {
        let ratio = self.hit_ratio();
        if ratio < 0.5 {
            // Low hit ratio: try stride detection
            self.read_ahead.set_strategy(PrefetchStrategy::StrideBased);
        } else if ratio > 0.9 {
            // High hit ratio: no need for aggressive prefetch
            self.read_ahead.set_strategy(PrefetchStrategy::None);
        } else {
            self.read_ahead.set_strategy(PrefetchStrategy::Sequential);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_k_access_and_distance() {
        let mut e = LruKEntry::new(1, 2);
        // Less than K accesses → max distance
        assert_eq!(e.backward_k_distance(), u64::MAX);
        e.access();
        assert_eq!(e.backward_k_distance(), u64::MAX); // still only 1
        e.access();
        // Now has K=2 accesses, distance should be small
        assert!(e.backward_k_distance() < 1_000_000); // < 1 second
    }

    #[test]
    fn lru_k_evictor_basic() {
        let mut ev = LruKEvictor::new(4, 2);
        assert!(ev.access(1)); // new page
        assert!(!ev.access(1)); // existing page
        ev.access(2);
        ev.access(3);
        assert_eq!(ev.len(), 3);
        assert!(ev.contains(2));
    }

    #[test]
    fn lru_k_select_victim() {
        let mut ev = LruKEvictor::new(4, 2);
        ev.access(1);
        ev.access(2);
        ev.access(3);
        // Pages with < K accesses have MAX distance; all are candidates
        let victim = ev.select_victim();
        assert!(victim.is_some());
    }

    #[test]
    fn lru_k_pin_prevents_eviction() {
        let mut ev = LruKEvictor::new(4, 2);
        ev.access(1);
        ev.access(2);
        ev.pin(1);
        ev.pin(2);
        // All pinned → no victim
        assert_eq!(ev.select_victim(), None);
        ev.unpin(1);
        assert_eq!(ev.select_victim(), Some(1));
    }

    #[test]
    fn lru_k_dirty_pages() {
        let mut ev = LruKEvictor::new(10, 2);
        ev.access(1);
        ev.access(2);
        ev.access(3);
        ev.mark_dirty(1);
        ev.mark_dirty(3);
        let dirty = ev.dirty_pages();
        assert_eq!(dirty.len(), 2);
        assert!(dirty.contains(&1));
        assert!(dirty.contains(&3));
    }

    #[test]
    fn read_ahead_sequential() {
        let mut ra = ReadAheadManager::new(PrefetchStrategy::Sequential, 3);
        let prefetch = ra.on_access(10);
        assert_eq!(prefetch, vec![11, 12, 13]);
    }

    #[test]
    fn read_ahead_none() {
        let mut ra = ReadAheadManager::new(PrefetchStrategy::None, 3);
        let prefetch = ra.on_access(10);
        assert!(prefetch.is_empty());
    }

    #[test]
    fn read_ahead_stride_detection() {
        let mut ra = ReadAheadManager::new(PrefetchStrategy::StrideBased, 2);
        ra.on_access(10);
        ra.on_access(15);
        let prefetch = ra.on_access(20); // stride = 5
        assert_eq!(prefetch, vec![25, 30]);
    }

    #[test]
    fn write_coalescer_batch_and_flush() {
        let mut wc = WriteCoalescer::new(3);
        assert!(!wc.add_write(1, vec![0; 100]));
        assert!(!wc.add_write(2, vec![0; 100]));
        assert!(wc.add_write(3, vec![0; 100])); // triggers flush threshold
        let (pages, bytes) = wc.flush();
        assert_eq!(pages.len(), 3);
        assert_eq!(bytes, 300);
        assert_eq!(wc.flush_count(), 1);
        assert_eq!(wc.bytes_flushed(), 300);
    }

    #[test]
    fn write_coalescer_dedup() {
        let mut wc = WriteCoalescer::new(10);
        wc.add_write(1, vec![0; 50]);
        wc.add_write(1, vec![0; 100]); // overwrites
        assert_eq!(wc.pending_count(), 1);
    }

    #[test]
    fn adaptive_pool_hit_ratio() {
        let mut pool = AdaptiveBufferPool::new(10, 2, 5);
        pool.access_page(1); // miss
        pool.access_page(1); // hit
        pool.access_page(1); // hit
        assert!(pool.hit_ratio() > 0.5);
    }

    #[test]
    fn adaptive_pool_adapt_strategy() {
        let mut pool = AdaptiveBufferPool::new(10, 2, 5);
        // Simulate low hit ratio
        for i in 0..20 {
            pool.access_page(i); // all misses
        }
        pool.adapt();
        assert_eq!(pool.read_ahead.strategy(), PrefetchStrategy::StrideBased);
    }

    #[test]
    fn adaptive_pool_eviction() {
        let mut pool = AdaptiveBufferPool::new(3, 1, 5);
        pool.access_page(1);
        pool.access_page(2);
        pool.access_page(3);
        let evicted = pool.evict_pages(2);
        assert_eq!(evicted.len(), 2);
        assert_eq!(pool.evictor.len(), 1);
    }
}
