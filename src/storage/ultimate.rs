// R17 – Storage engine ultimate: adaptive page size, WAL group commit,
//       space reclamation/defragmentation, storage histograms, parallel checkpoint.
//
// Provides:
//   - `AdaptivePageSize`: dynamically selects page size based on workload
//   - `WalGroupCommit`: batches WAL flushes for throughput
//   - `SpaceReclaimer`: tracks free space, triggers defragmentation
//   - `StorageHistogram`: equi-depth histogram for column statistics
//   - `ParallelCheckpoint`: concurrent checkpoint coordination

use std::collections::HashMap;

// ── Adaptive Page Size ────────────────────────────────────────────────

/// Page size options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageSize {
    Small = 4096,
    Medium = 8192,
    Large = 16384,
    Huge = 32768,
}

impl PageSize {
    pub fn bytes(&self) -> usize {
        *self as usize
    }
}

/// Tracks per-table optimal page sizes.
pub struct AdaptivePageSize {
    table_sizes: HashMap<String, PageSize>,
    default_size: PageSize,
    /// Average row size per table for decision-making.
    avg_row_sizes: HashMap<String, usize>,
}

impl AdaptivePageSize {
    pub fn new(default: PageSize) -> Self {
        Self {
            table_sizes: HashMap::new(),
            default_size: default,
            avg_row_sizes: HashMap::new(),
        }
    }

    /// Record average row size and auto-select page size.
    pub fn observe_row_size(&mut self, table: &str, avg_row_bytes: usize) {
        self.avg_row_sizes.insert(table.to_string(), avg_row_bytes);
        let page_size = if avg_row_bytes > 8192 {
            PageSize::Huge
        } else if avg_row_bytes > 2048 {
            PageSize::Large
        } else if avg_row_bytes > 512 {
            PageSize::Medium
        } else {
            PageSize::Small
        };
        self.table_sizes.insert(table.to_string(), page_size);
    }

    pub fn get_page_size(&self, table: &str) -> PageSize {
        self.table_sizes.get(table).copied().unwrap_or(self.default_size)
    }

    pub fn set_page_size(&mut self, table: &str, size: PageSize) {
        self.table_sizes.insert(table.to_string(), size);
    }

    pub fn table_count(&self) -> usize {
        self.table_sizes.len()
    }
}

// ── WAL Group Commit ──────────────────────────────────────────────────

/// A pending WAL write.
#[derive(Debug, Clone)]
pub struct WalEntry {
    pub txn_id: u64,
    pub lsn: u64,
    pub data_size: usize,
}

/// Batches WAL flushes for higher throughput.
pub struct WalGroupCommit {
    pending: Vec<WalEntry>,
    max_batch: usize,
    max_wait_us: u64,
    flush_count: u64,
    total_entries_flushed: u64,
    next_lsn: u64,
}

impl WalGroupCommit {
    pub fn new(max_batch: usize, max_wait_us: u64) -> Self {
        Self {
            pending: Vec::new(),
            max_batch,
            max_wait_us,
            flush_count: 0,
            total_entries_flushed: 0,
            next_lsn: 1,
        }
    }

    /// Add a WAL entry. Returns true if a flush should be triggered.
    pub fn add(&mut self, txn_id: u64, data_size: usize) -> bool {
        let lsn = self.next_lsn;
        self.next_lsn += 1;
        self.pending.push(WalEntry { txn_id, lsn, data_size });
        self.pending.len() >= self.max_batch
    }

    /// Flush all pending entries.
    pub fn flush(&mut self) -> Vec<WalEntry> {
        let entries = std::mem::take(&mut self.pending);
        self.flush_count += 1;
        self.total_entries_flushed += entries.len() as u64;
        entries
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn flush_count(&self) -> u64 {
        self.flush_count
    }

    pub fn avg_batch_size(&self) -> f64 {
        if self.flush_count == 0 { return 0.0; }
        self.total_entries_flushed as f64 / self.flush_count as f64
    }
}

// ── Space Reclamation ─────────────────────────────────────────────────

/// Free space fragmentation info for a page.
#[derive(Debug, Clone)]
pub struct PageFreeSpace {
    pub page_id: u32,
    pub total_bytes: usize,
    pub used_bytes: usize,
    pub fragment_count: usize,
}

impl PageFreeSpace {
    pub fn free_bytes(&self) -> usize {
        self.total_bytes.saturating_sub(self.used_bytes)
    }

    pub fn utilization(&self) -> f64 {
        if self.total_bytes == 0 { return 0.0; }
        self.used_bytes as f64 / self.total_bytes as f64
    }
}

/// Manages space reclamation and defragmentation.
pub struct SpaceReclaimer {
    pages: HashMap<u32, PageFreeSpace>,
    /// Threshold below which a page is considered for compaction.
    compaction_threshold: f64,
    compactions_done: u64,
}

impl SpaceReclaimer {
    pub fn new(compaction_threshold: f64) -> Self {
        Self {
            pages: HashMap::new(),
            compaction_threshold,
            compactions_done: 0,
        }
    }

    /// Register or update a page's free space info.
    pub fn update_page(&mut self, page_id: u32, total: usize, used: usize, fragments: usize) {
        self.pages.insert(page_id, PageFreeSpace {
            page_id,
            total_bytes: total,
            used_bytes: used,
            fragment_count: fragments,
        });
    }

    /// Find pages that need compaction (low utilization).
    pub fn pages_needing_compaction(&self) -> Vec<u32> {
        self.pages.values()
            .filter(|p| p.utilization() < self.compaction_threshold)
            .map(|p| p.page_id)
            .collect()
    }

    /// Simulate compaction of a page.
    pub fn compact_page(&mut self, page_id: u32) -> bool {
        if let Some(page) = self.pages.get_mut(&page_id) {
            page.fragment_count = 1; // defragmented to one contiguous block
            self.compactions_done += 1;
            true
        } else {
            false
        }
    }

    /// Total wasted space across all pages.
    pub fn total_wasted(&self) -> usize {
        self.pages.values().map(|p| p.free_bytes()).sum()
    }

    pub fn compactions_done(&self) -> u64 {
        self.compactions_done
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

// ── Storage Histogram ─────────────────────────────────────────────────

/// Equi-depth histogram bucket.
#[derive(Debug, Clone)]
pub struct HistogramBucket {
    pub lower_bound: i64,
    pub upper_bound: i64,
    pub frequency: u64,
    pub distinct_count: u64,
}

/// Column statistics histogram.
pub struct StorageHistogram {
    pub column_name: String,
    buckets: Vec<HistogramBucket>,
    total_rows: u64,
    null_count: u64,
}

impl StorageHistogram {
    pub fn new(column_name: &str) -> Self {
        Self {
            column_name: column_name.to_string(),
            buckets: Vec::new(),
            total_rows: 0,
            null_count: 0,
        }
    }

    /// Build histogram from sorted values.
    pub fn build_from_sorted(&mut self, values: &[i64], num_buckets: usize) {
        if values.is_empty() || num_buckets == 0 { return; }
        self.total_rows = values.len() as u64;
        self.buckets.clear();

        let bucket_size = (values.len() + num_buckets - 1) / num_buckets;
        for chunk in values.chunks(bucket_size) {
            let lower = *chunk.first().unwrap();
            let upper = *chunk.last().unwrap();
            let mut distinct: Vec<i64> = chunk.to_vec();
            distinct.sort();
            distinct.dedup();
            self.buckets.push(HistogramBucket {
                lower_bound: lower,
                upper_bound: upper,
                frequency: chunk.len() as u64,
                distinct_count: distinct.len() as u64,
            });
        }
    }

    /// Estimate selectivity for an equality predicate.
    pub fn estimate_eq_selectivity(&self, value: i64) -> f64 {
        if self.total_rows == 0 { return 0.0; }
        for bucket in &self.buckets {
            if value >= bucket.lower_bound && value <= bucket.upper_bound {
                if bucket.distinct_count == 0 { return 0.0; }
                return 1.0 / bucket.distinct_count as f64;
            }
        }
        1.0 / self.total_rows as f64 // not found, assume uniform
    }

    /// Estimate selectivity for a range predicate [lo, hi].
    pub fn estimate_range_selectivity(&self, lo: i64, hi: i64) -> f64 {
        if self.total_rows == 0 { return 0.0; }
        let mut matching = 0u64;
        for bucket in &self.buckets {
            if bucket.upper_bound >= lo && bucket.lower_bound <= hi {
                matching += bucket.frequency;
            }
        }
        matching as f64 / self.total_rows as f64
    }

    pub fn set_null_count(&mut self, count: u64) {
        self.null_count = count;
    }

    pub fn null_fraction(&self) -> f64 {
        if self.total_rows == 0 { return 0.0; }
        self.null_count as f64 / self.total_rows as f64
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

// ── Parallel Checkpoint ───────────────────────────────────────────────

/// Checkpoint worker state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkerState {
    Idle,
    Flushing,
    Completed,
    Failed,
}

/// A checkpoint worker.
#[derive(Debug, Clone)]
pub struct CheckpointWorker {
    pub worker_id: u32,
    pub state: WorkerState,
    pub pages_flushed: usize,
    pub assigned_pages: Vec<u32>,
}

/// Coordinates parallel checkpoint execution.
pub struct ParallelCheckpoint {
    workers: Vec<CheckpointWorker>,
    checkpoint_lsn: u64,
    is_active: bool,
    total_pages_flushed: usize,
}

impl ParallelCheckpoint {
    pub fn new(num_workers: u32) -> Self {
        let workers = (0..num_workers).map(|i| CheckpointWorker {
            worker_id: i,
            state: WorkerState::Idle,
            pages_flushed: 0,
            assigned_pages: Vec::new(),
        }).collect();
        Self {
            workers,
            checkpoint_lsn: 0,
            is_active: false,
            total_pages_flushed: 0,
        }
    }

    /// Start a checkpoint: distribute dirty pages among workers.
    pub fn start(&mut self, dirty_pages: Vec<u32>, lsn: u64) {
        self.checkpoint_lsn = lsn;
        self.is_active = true;
        let num_workers = self.workers.len();
        for (i, page) in dirty_pages.iter().enumerate() {
            let w = i % num_workers;
            self.workers[w].assigned_pages.push(*page);
            self.workers[w].state = WorkerState::Flushing;
        }
    }

    /// Worker completes flushing its pages.
    pub fn worker_complete(&mut self, worker_id: u32) -> bool {
        if let Some(w) = self.workers.iter_mut().find(|w| w.worker_id == worker_id) {
            w.pages_flushed = w.assigned_pages.len();
            self.total_pages_flushed += w.pages_flushed;
            w.state = WorkerState::Completed;
            true
        } else {
            false
        }
    }

    /// Check if all workers are done.
    pub fn is_complete(&self) -> bool {
        self.workers.iter().all(|w| w.state == WorkerState::Completed || w.state == WorkerState::Idle)
    }

    /// Finish the checkpoint.
    pub fn finish(&mut self) {
        for w in &mut self.workers {
            w.state = WorkerState::Idle;
            w.assigned_pages.clear();
            w.pages_flushed = 0;
        }
        self.is_active = false;
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn total_pages_flushed(&self) -> usize {
        self.total_pages_flushed
    }

    pub fn checkpoint_lsn(&self) -> u64 {
        self.checkpoint_lsn
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_page_size_auto() {
        let mut aps = AdaptivePageSize::new(PageSize::Small);
        aps.observe_row_size("big_table", 5000);
        assert_eq!(aps.get_page_size("big_table"), PageSize::Large);
        aps.observe_row_size("tiny_table", 100);
        assert_eq!(aps.get_page_size("tiny_table"), PageSize::Small);
        assert_eq!(aps.table_count(), 2);
    }

    #[test]
    fn wal_group_commit_batching() {
        let mut gc = WalGroupCommit::new(3, 1000);
        assert!(!gc.add(1, 100));
        assert!(!gc.add(2, 100));
        assert!(gc.add(3, 100)); // 3rd → triggers flush
        let flushed = gc.flush();
        assert_eq!(flushed.len(), 3);
        assert_eq!(gc.flush_count(), 1);
    }

    #[test]
    fn space_reclaimer_compaction() {
        let mut sr = SpaceReclaimer::new(0.5);
        sr.update_page(1, 4096, 3000, 2);
        sr.update_page(2, 4096, 1000, 5); // 24% util → needs compaction
        let needing = sr.pages_needing_compaction();
        assert!(needing.contains(&2));
        assert!(!needing.contains(&1));
        sr.compact_page(2);
        assert_eq!(sr.compactions_done(), 1);
    }

    #[test]
    fn storage_histogram_selectivity() {
        let mut hist = StorageHistogram::new("age");
        let values: Vec<i64> = (0..100).collect();
        hist.build_from_sorted(&values, 10);
        assert_eq!(hist.bucket_count(), 10);
        let sel = hist.estimate_eq_selectivity(50);
        assert!(sel > 0.0 && sel < 1.0);
        let range_sel = hist.estimate_range_selectivity(20, 40);
        assert!(range_sel > 0.1);
    }

    #[test]
    fn parallel_checkpoint_lifecycle() {
        let mut cp = ParallelCheckpoint::new(3);
        cp.start(vec![1, 2, 3, 4, 5, 6], 100);
        for i in 0..3 { cp.worker_complete(i); }
        assert!(cp.is_complete());
        assert_eq!(cp.total_pages_flushed(), 6);
        cp.finish();
        assert_eq!(cp.checkpoint_lsn(), 100);
    }
}
