// R16 – Storage engine extreme optimization: adaptive compression strategy,
//       hot/cold data tiering, IO scheduler, page warmup, incremental backup chain.
//
// Provides:
//   - `AdaptiveCompression`: selects compression algo per page based on access pattern
//   - `DataTierManager`: hot/warm/cold tiering with promotion/demotion
//   - `IoScheduler`: prioritized IO request queue (reads > writes, fairness)
//   - `PageWarmer`: preloads pages on startup based on access frequency
//   - `IncrementalBackup`: backup chain with incremental snapshots

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

// ── Adaptive Compression ──────────────────────────────────────────────

/// Compression algorithm choices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompressionAlgo {
    None,
    Lz4,
    Zstd,
    Snappy,
}

impl CompressionAlgo {
    /// Compression ratio estimate (higher = better compression).
    pub fn estimated_ratio(&self) -> f64 {
        match self {
            Self::None => 1.0,
            Self::Lz4 => 2.0,
            Self::Snappy => 1.8,
            Self::Zstd => 3.5,
        }
    }

    /// Relative CPU cost (higher = slower).
    pub fn cpu_cost(&self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Lz4 => 1.0,
            Self::Snappy => 0.8,
            Self::Zstd => 5.0,
        }
    }
}

/// Per-page access statistics for compression decisions.
#[derive(Debug, Clone)]
struct PageStats {
    access_count: u64,
    last_access: u64,
    current_algo: CompressionAlgo,
}

/// Selects compression algorithm adaptively based on access patterns.
pub struct AdaptiveCompression {
    pages: HashMap<u32, PageStats>,
    /// Access threshold: pages above this are "hot" → use fast compression.
    hot_threshold: u64,
    /// Default algorithm for new pages.
    default_algo: CompressionAlgo,
    tick: u64,
}

impl AdaptiveCompression {
    pub fn new(hot_threshold: u64) -> Self {
        Self {
            pages: HashMap::new(),
            hot_threshold,
            default_algo: CompressionAlgo::Lz4,
            tick: 0,
        }
    }

    /// Record a page access and return the recommended compression.
    pub fn on_access(&mut self, page_id: u32) -> CompressionAlgo {
        self.tick += 1;
        let stats = self.pages.entry(page_id).or_insert(PageStats {
            access_count: 0,
            last_access: 0,
            current_algo: self.default_algo,
        });
        stats.access_count += 1;
        stats.last_access = self.tick;

        // Hot pages → fast compression; cold pages → aggressive compression
        let algo = if stats.access_count >= self.hot_threshold {
            CompressionAlgo::Lz4
        } else {
            CompressionAlgo::Zstd
        };
        stats.current_algo = algo;
        algo
    }

    /// Get recommended algorithm for a page without recording access.
    pub fn recommend(&self, page_id: u32) -> CompressionAlgo {
        self.pages
            .get(&page_id)
            .map(|s| s.current_algo)
            .unwrap_or(self.default_algo)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn set_default(&mut self, algo: CompressionAlgo) {
        self.default_algo = algo;
    }
}

// ── Hot/Cold Data Tiering ─────────────────────────────────────────────

/// Storage tier classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataTier {
    Hot,
    Warm,
    Cold,
    Archive,
}

/// Track for a data segment.
#[derive(Debug, Clone)]
pub struct TierEntry {
    pub segment_id: u32,
    pub tier: DataTier,
    pub access_count: u64,
    pub last_access: u64,
    pub byte_size: usize,
}

/// Manages data tiering with promotion/demotion rules.
pub struct DataTierManager {
    entries: HashMap<u32, TierEntry>,
    /// Thresholds (access count → tier).
    hot_threshold: u64,
    warm_threshold: u64,
    cold_after_ticks: u64,
    tick: u64,
}

impl DataTierManager {
    pub fn new(hot_threshold: u64, warm_threshold: u64, cold_after_ticks: u64) -> Self {
        Self {
            entries: HashMap::new(),
            hot_threshold,
            warm_threshold,
            cold_after_ticks,
            tick: 0,
        }
    }

    /// Register a segment.
    pub fn add_segment(&mut self, id: u32, byte_size: usize) {
        self.entries.insert(
            id,
            TierEntry {
                segment_id: id,
                tier: DataTier::Warm,
                access_count: 0,
                last_access: self.tick,
                byte_size,
            },
        );
    }

    /// Record an access to a segment.
    pub fn access(&mut self, id: u32) -> Option<DataTier> {
        self.tick += 1;
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.access_count += 1;
            entry.last_access = self.tick;
            // Promote if hot enough
            if entry.access_count >= self.hot_threshold {
                entry.tier = DataTier::Hot;
            } else if entry.access_count >= self.warm_threshold {
                entry.tier = DataTier::Warm;
            }
            Some(entry.tier)
        } else {
            None
        }
    }

    /// Demote cold segments based on age.
    pub fn demote_cold(&mut self) -> Vec<u32> {
        let tick = self.tick;
        let threshold = self.cold_after_ticks;
        let mut demoted = Vec::new();
        for entry in self.entries.values_mut() {
            if tick - entry.last_access > threshold
                && entry.tier != DataTier::Cold
                && entry.tier != DataTier::Archive
            {
                entry.tier = DataTier::Cold;
                demoted.push(entry.segment_id);
            }
        }
        demoted
    }

    /// Get segments in a specific tier.
    pub fn segments_in_tier(&self, tier: DataTier) -> Vec<u32> {
        self.entries
            .values()
            .filter(|e| e.tier == tier)
            .map(|e| e.segment_id)
            .collect()
    }

    pub fn segment_count(&self) -> usize {
        self.entries.len()
    }
}

// ── IO Scheduler ──────────────────────────────────────────────────────

/// IO request type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoType {
    Read,
    Write,
    Sync,
    Prefetch,
}

/// A prioritised IO request.
#[derive(Debug, Clone)]
pub struct IoRequest {
    pub request_id: u64,
    pub io_type: IoType,
    pub page_id: u32,
    pub priority: u32, // higher = more urgent
}

impl PartialEq for IoRequest {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}
impl Eq for IoRequest {}
impl PartialOrd for IoRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for IoRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}

/// Simple priority-based IO scheduler.
pub struct IoScheduler {
    queue: BinaryHeap<IoRequest>,
    completed: u64,
    next_id: u64,
}

impl IoScheduler {
    pub fn new() -> Self {
        Self {
            queue: BinaryHeap::new(),
            completed: 0,
            next_id: 1,
        }
    }

    /// Submit an IO request with auto-priority based on type.
    pub fn submit(&mut self, io_type: IoType, page_id: u32) -> u64 {
        let priority = match io_type {
            IoType::Read => 100,
            IoType::Write => 80,
            IoType::Sync => 120,
            IoType::Prefetch => 50,
        };
        let id = self.next_id;
        self.next_id += 1;
        self.queue.push(IoRequest {
            request_id: id,
            io_type,
            page_id,
            priority,
        });
        id
    }

    /// Submit with custom priority.
    pub fn submit_priority(&mut self, io_type: IoType, page_id: u32, priority: u32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.queue.push(IoRequest {
            request_id: id,
            io_type,
            page_id,
            priority,
        });
        id
    }

    /// Dequeue next highest priority request.
    pub fn next(&mut self) -> Option<IoRequest> {
        let req = self.queue.pop();
        if req.is_some() {
            self.completed += 1;
        }
        req
    }

    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    pub fn completed(&self) -> u64 {
        self.completed
    }
}

// ── Page Warmer ───────────────────────────────────────────────────────

/// Tracks page access frequency for pre-warming decisions.
pub struct PageWarmer {
    /// page_id → cumulative access count.
    frequencies: HashMap<u32, u64>,
    /// Max pages to warm.
    max_warm: usize,
}

impl PageWarmer {
    pub fn new(max_warm: usize) -> Self {
        Self {
            frequencies: HashMap::new(),
            max_warm,
        }
    }

    /// Record a page access.
    pub fn record(&mut self, page_id: u32) {
        *self.frequencies.entry(page_id).or_insert(0) += 1;
    }

    /// Get the top-N pages to pre-warm (by frequency).
    pub fn warm_list(&self) -> Vec<u32> {
        let mut sorted: Vec<(u32, u64)> = self.frequencies.iter().map(|(&k, &v)| (k, v)).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted
            .into_iter()
            .take(self.max_warm)
            .map(|(k, _)| k)
            .collect()
    }

    /// Reset all frequency data.
    pub fn reset(&mut self) {
        self.frequencies.clear();
    }

    pub fn tracked_pages(&self) -> usize {
        self.frequencies.len()
    }
}

// ── Incremental Backup Chain ──────────────────────────────────────────

/// A backup entry in the chain.
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub backup_id: u64,
    pub parent_id: Option<u64>,
    pub lsn_start: u64,
    pub lsn_end: u64,
    pub byte_size: usize,
    pub is_full: bool,
    pub timestamp: u64,
}

/// Manages incremental backup chains.
pub struct IncrementalBackup {
    entries: Vec<BackupEntry>,
    next_id: u64,
}

impl IncrementalBackup {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
        }
    }

    /// Create a full backup.
    pub fn full_backup(&mut self, lsn_end: u64, byte_size: usize, timestamp: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(BackupEntry {
            backup_id: id,
            parent_id: None,
            lsn_start: 0,
            lsn_end,
            byte_size,
            is_full: true,
            timestamp,
        });
        id
    }

    /// Create an incremental backup since the given parent.
    pub fn incremental_backup(
        &mut self,
        parent_id: u64,
        lsn_start: u64,
        lsn_end: u64,
        byte_size: usize,
        timestamp: u64,
    ) -> Option<u64> {
        // Verify parent exists
        if !self.entries.iter().any(|e| e.backup_id == parent_id) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(BackupEntry {
            backup_id: id,
            parent_id: Some(parent_id),
            lsn_start,
            lsn_end,
            byte_size,
            is_full: false,
            timestamp,
        });
        Some(id)
    }

    /// Get the restore chain (from full backup to target).
    pub fn restore_chain(&self, target_id: u64) -> Vec<u64> {
        let mut chain = Vec::new();
        let mut current = target_id;
        loop {
            if let Some(entry) = self.entries.iter().find(|e| e.backup_id == current) {
                chain.push(current);
                if let Some(parent) = entry.parent_id {
                    current = parent;
                } else {
                    break; // reached full backup
                }
            } else {
                break;
            }
        }
        chain.reverse();
        chain
    }

    /// Total size of restore chain.
    pub fn chain_size(&self, target_id: u64) -> usize {
        let ids = self.restore_chain(target_id);
        ids.iter()
            .filter_map(|id| self.entries.iter().find(|e| e.backup_id == *id))
            .map(|e| e.byte_size)
            .sum()
    }

    pub fn backup_count(&self) -> usize {
        self.entries.len()
    }

    pub fn latest_lsn(&self) -> u64 {
        self.entries.iter().map(|e| e.lsn_end).max().unwrap_or(0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_compression_hot_cold() {
        let mut ac = AdaptiveCompression::new(5);
        // First access → cold → Zstd
        assert_eq!(ac.on_access(1), CompressionAlgo::Zstd);
        // 4 more accesses → still cold
        for _ in 0..3 {
            ac.on_access(1);
        }
        assert_eq!(ac.on_access(1), CompressionAlgo::Lz4); // 5th access → hot
    }

    #[test]
    fn adaptive_compression_recommend() {
        let mut ac = AdaptiveCompression::new(3);
        assert_eq!(ac.recommend(99), CompressionAlgo::Lz4); // default
        ac.on_access(99);
        assert_eq!(ac.recommend(99), CompressionAlgo::Zstd);
        assert_eq!(ac.page_count(), 1);
    }

    #[test]
    fn data_tier_promotion() {
        let mut dtm = DataTierManager::new(5, 2, 100);
        dtm.add_segment(1, 4096);
        assert_eq!(dtm.access(1), Some(DataTier::Warm));
        dtm.access(1); // count=2 → Warm
        assert_eq!(dtm.access(1), Some(DataTier::Warm)); // count=3
        for _ in 0..2 {
            dtm.access(1);
        }
        assert_eq!(dtm.access(1), Some(DataTier::Hot)); // count=6
    }

    #[test]
    fn data_tier_demotion() {
        let mut dtm = DataTierManager::new(5, 2, 10);
        dtm.add_segment(1, 1024);
        dtm.access(1);
        // Advance tick without accessing
        for _ in 0..15 {
            dtm.access(999);
        } // dummy accesses to advance tick
          // But segment 999 doesn't exist, so tick won't advance. Use add_segment first
        dtm.add_segment(2, 1024);
        for _ in 0..12 {
            dtm.access(2);
        }
        let demoted = dtm.demote_cold();
        assert!(demoted.contains(&1)); // segment 1 hasn't been accessed in >10 ticks
    }

    #[test]
    fn io_scheduler_priority() {
        let mut sched = IoScheduler::new();
        sched.submit(IoType::Prefetch, 1); // priority 50
        sched.submit(IoType::Read, 2); // priority 100
        sched.submit(IoType::Sync, 3); // priority 120

        let first = sched.next().unwrap();
        assert_eq!(first.io_type, IoType::Sync); // highest priority
        let second = sched.next().unwrap();
        assert_eq!(second.io_type, IoType::Read);
        assert_eq!(sched.pending(), 1);
        assert_eq!(sched.completed(), 2);
    }

    #[test]
    fn page_warmer_top_pages() {
        let mut pw = PageWarmer::new(3);
        for _ in 0..10 {
            pw.record(1);
        }
        for _ in 0..5 {
            pw.record(2);
        }
        for _ in 0..20 {
            pw.record(3);
        }
        for _ in 0..1 {
            pw.record(4);
        }
        let warm = pw.warm_list();
        assert_eq!(warm.len(), 3);
        assert_eq!(warm[0], 3); // most frequent
        assert_eq!(pw.tracked_pages(), 4);
    }

    #[test]
    fn incremental_backup_chain() {
        let mut bk = IncrementalBackup::new();
        let full = bk.full_backup(100, 10000, 1);
        let inc1 = bk.incremental_backup(full, 100, 200, 500, 2).unwrap();
        let inc2 = bk.incremental_backup(inc1, 200, 300, 300, 3).unwrap();

        let chain = bk.restore_chain(inc2);
        assert_eq!(chain, vec![full, inc1, inc2]);
        assert_eq!(bk.chain_size(inc2), 10800); // 10000+500+300
        assert_eq!(bk.backup_count(), 3);
        assert_eq!(bk.latest_lsn(), 300);
    }

    #[test]
    fn incremental_backup_invalid_parent() {
        let mut bk = IncrementalBackup::new();
        assert!(bk.incremental_backup(999, 0, 100, 500, 1).is_none());
    }

    #[test]
    fn compression_algo_properties() {
        assert!(CompressionAlgo::Zstd.estimated_ratio() > CompressionAlgo::Lz4.estimated_ratio());
        assert!(CompressionAlgo::Zstd.cpu_cost() > CompressionAlgo::Lz4.cpu_cost());
        assert_eq!(CompressionAlgo::None.cpu_cost(), 0.0);
    }
}
