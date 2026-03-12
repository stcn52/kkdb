// ── src/storage/ext/deep_storage.rs ──
// R22: 存储引擎深层优化 — 列存储引擎 / 数据分区管理 / 冷热数据分层 / 存储空间回收

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
// 1. ColumnarEngine — 列存储引擎
// ═══════════════════════════════════════════════════════════════════════

/// 列编码方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnEncoding {
    Plain,
    RunLength,
    Dictionary,
    DeltaBinary,
    BitPacked,
}

/// 列存储段
#[derive(Debug, Clone)]
pub struct ColumnSegment {
    pub column_name: String,
    pub encoding: ColumnEncoding,
    pub row_count: usize,
    pub null_count: usize,
    pub min_value: i64,
    pub max_value: i64,
    pub compressed_size: usize,
    pub uncompressed_size: usize,
}

impl ColumnSegment {
    pub fn compression_ratio(&self) -> f64 {
        if self.uncompressed_size == 0 {
            return 1.0;
        }
        self.compressed_size as f64 / self.uncompressed_size as f64
    }
}

/// 列存储引擎
pub struct ColumnarEngine {
    segments: HashMap<String, Vec<ColumnSegment>>,
    row_groups: Vec<RowGroup>,
    #[allow(dead_code)]
    row_group_size: usize,
    total_rows: usize,
}

/// 行组（列存储的基本单位）
#[derive(Debug, Clone)]
pub struct RowGroup {
    pub id: usize,
    pub row_count: usize,
    pub columns: Vec<String>,
    pub size_bytes: usize,
}

impl ColumnarEngine {
    pub fn new(row_group_size: usize) -> Self {
        Self {
            segments: HashMap::new(),
            row_groups: Vec::new(),
            row_group_size,
            total_rows: 0,
        }
    }

    pub fn add_segment(&mut self, seg: ColumnSegment) {
        self.segments
            .entry(seg.column_name.clone())
            .or_default()
            .push(seg);
    }

    pub fn create_row_group(&mut self, columns: Vec<&str>, row_count: usize) -> usize {
        let id = self.row_groups.len();
        self.row_groups.push(RowGroup {
            id,
            row_count,
            columns: columns.iter().map(|s| s.to_string()).collect(),
            size_bytes: 0,
        });
        self.total_rows += row_count;
        id
    }

    /// 仅扫描指定列（列裁剪）
    pub fn project_scan(&self, columns: &[&str]) -> Vec<&ColumnSegment> {
        let mut result = Vec::new();
        for col in columns {
            if let Some(segs) = self.segments.get(*col) {
                result.extend(segs.iter());
            }
        }
        result
    }

    /// 基于 min/max 做段跳过
    pub fn segment_skip(&self, column: &str, min_val: i64, max_val: i64) -> Vec<&ColumnSegment> {
        match self.segments.get(column) {
            Some(segs) => segs
                .iter()
                .filter(|s| s.max_value >= min_val && s.min_value <= max_val)
                .collect(),
            None => vec![],
        }
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    pub fn row_group_count(&self) -> usize {
        self.row_groups.len()
    }

    pub fn column_count(&self) -> usize {
        self.segments.len()
    }

    pub fn avg_compression(&self) -> f64 {
        let segs: Vec<&ColumnSegment> = self.segments.values().flat_map(|v| v.iter()).collect();
        if segs.is_empty() {
            return 1.0;
        }
        let total_ratio: f64 = segs.iter().map(|s| s.compression_ratio()).sum();
        total_ratio / segs.len() as f64
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. PartitionManager — 数据分区管理
// ═══════════════════════════════════════════════════════════════════════

/// 分区策略
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionScheme {
    Range {
        column: String,
        boundaries: Vec<i64>,
    },
    Hash {
        column: String,
        num_buckets: usize,
    },
    List {
        column: String,
        values: Vec<Vec<String>>,
    },
}

/// 分区信息
#[derive(Debug, Clone)]
pub struct Partition {
    pub id: usize,
    pub name: String,
    pub scheme_index: usize,
    pub row_count: usize,
    pub size_bytes: usize,
    pub is_active: bool,
}

/// 分区管理器
pub struct PartitionManager {
    partitions: Vec<Partition>,
    schemes: Vec<PartitionScheme>,
    next_partition_id: usize,
}

impl Default for PartitionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PartitionManager {
    pub fn new() -> Self {
        Self {
            partitions: Vec::new(),
            schemes: Vec::new(),
            next_partition_id: 1,
        }
    }

    pub fn add_scheme(&mut self, scheme: PartitionScheme) -> usize {
        let idx = self.schemes.len();
        self.schemes.push(scheme);
        idx
    }

    pub fn create_partition(&mut self, name: &str, scheme_idx: usize) -> usize {
        let id = self.next_partition_id;
        self.next_partition_id += 1;
        self.partitions.push(Partition {
            id,
            name: name.to_string(),
            scheme_index: scheme_idx,
            row_count: 0,
            size_bytes: 0,
            is_active: true,
        });
        id
    }

    pub fn add_rows(&mut self, partition_id: usize, count: usize, bytes: usize) {
        if let Some(p) = self.partitions.iter_mut().find(|p| p.id == partition_id) {
            p.row_count += count;
            p.size_bytes += bytes;
        }
    }

    pub fn deactivate(&mut self, partition_id: usize) {
        if let Some(p) = self.partitions.iter_mut().find(|p| p.id == partition_id) {
            p.is_active = false;
        }
    }

    /// 分区裁剪：Range分区根据值范围过滤
    pub fn prune_range(&self, scheme_idx: usize, min_val: i64, max_val: i64) -> Vec<usize> {
        if let Some(PartitionScheme::Range { boundaries, .. }) = self.schemes.get(scheme_idx) {
            let mut ids = Vec::new();
            for (i, p) in self.partitions.iter().enumerate() {
                if p.scheme_index == scheme_idx && p.is_active {
                    let lower = if i == 0 {
                        i64::MIN
                    } else {
                        boundaries.get(i - 1).copied().unwrap_or(i64::MIN)
                    };
                    let upper = boundaries.get(i).copied().unwrap_or(i64::MAX);
                    if lower <= max_val && upper >= min_val {
                        ids.push(p.id);
                    }
                }
            }
            ids
        } else {
            self.partitions
                .iter()
                .filter(|p| p.is_active)
                .map(|p| p.id)
                .collect()
        }
    }

    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }

    pub fn active_partitions(&self) -> usize {
        self.partitions.iter().filter(|p| p.is_active).count()
    }

    pub fn total_rows(&self) -> usize {
        self.partitions.iter().map(|p| p.row_count).sum()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. TierManager — 冷热数据分层
// ═══════════════════════════════════════════════════════════════════════

/// 数据温度等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataTier {
    Hot,    // 频繁访问 — 内存/SSD
    Warm,   // 偶尔访问 — SSD
    Cold,   // 很少访问 — HDD/对象存储
    Frozen, // 几乎不访问 — 归档
}

/// 数据块的温度信息
#[derive(Debug, Clone)]
pub struct TierBlock {
    pub block_id: u64,
    pub tier: DataTier,
    pub last_access_ms: u64,
    pub access_count: u64,
    pub size_bytes: usize,
    pub created_ms: u64,
}

/// 冷热数据分层管理器
pub struct TierManager {
    blocks: Vec<TierBlock>,
    hot_threshold: u64,  // access count to stay hot
    warm_threshold: u64, // access count to stay warm
    cold_age_ms: u64,    // age to become cold
    promotions: u64,
    demotions: u64,
}

impl TierManager {
    pub fn new(hot_threshold: u64, warm_threshold: u64, cold_age_ms: u64) -> Self {
        Self {
            blocks: Vec::new(),
            hot_threshold,
            warm_threshold,
            cold_age_ms,
            promotions: 0,
            demotions: 0,
        }
    }

    pub fn register_block(&mut self, block_id: u64, size_bytes: usize, created_ms: u64) {
        self.blocks.push(TierBlock {
            block_id,
            tier: DataTier::Hot,
            last_access_ms: created_ms,
            access_count: 0,
            size_bytes,
            created_ms,
        });
    }

    pub fn access(&mut self, block_id: u64, timestamp_ms: u64) {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.block_id == block_id) {
            b.access_count += 1;
            b.last_access_ms = timestamp_ms;
            // Promote if cold/warm and accessed enough
            if b.tier != DataTier::Hot && b.access_count >= self.hot_threshold {
                b.tier = DataTier::Hot;
                self.promotions += 1;
            }
        }
    }

    /// 根据访问模式重新分层
    pub fn rebalance(&mut self, current_ms: u64) {
        for block in &mut self.blocks {
            let age = current_ms.saturating_sub(block.last_access_ms);
            let old_tier = block.tier;

            if block.access_count >= self.hot_threshold {
                block.tier = DataTier::Hot;
            } else if block.access_count >= self.warm_threshold {
                block.tier = DataTier::Warm;
            } else if age >= self.cold_age_ms * 2 {
                block.tier = DataTier::Frozen;
            } else if age >= self.cold_age_ms {
                block.tier = DataTier::Cold;
            }

            if block.tier as u8 > old_tier as u8 {
                self.demotions += 1;
            } else if (block.tier as u8) < (old_tier as u8) {
                self.promotions += 1;
            }
        }
    }

    pub fn tier_summary(&self) -> HashMap<DataTier, (usize, usize)> {
        let mut summary: HashMap<DataTier, (usize, usize)> = HashMap::new();
        for b in &self.blocks {
            let entry = summary.entry(b.tier).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += b.size_bytes;
        }
        summary
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn promotions(&self) -> u64 {
        self.promotions
    }

    pub fn demotions(&self) -> u64 {
        self.demotions
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. SpaceReclaimer — 存储空间回收
// ═══════════════════════════════════════════════════════════════════════

/// 空闲页面池
#[derive(Debug, Clone)]
pub struct FreePage {
    pub page_id: u32,
    pub freed_at_txn: u64,
    pub size_bytes: usize,
}

/// 回收统计
#[derive(Debug, Clone, Default)]
pub struct ReclaimStats {
    pub pages_freed: u64,
    pub bytes_reclaimed: u64,
    pub compactions_run: u64,
    pub fragmentation_pct: f64,
}

/// 存储空间回收器
pub struct SpaceReclaimer {
    free_pages: Vec<FreePage>,
    stats: ReclaimStats,
    total_pages: u64,
    used_pages: u64,
}

impl SpaceReclaimer {
    pub fn new(total_pages: u64) -> Self {
        Self {
            free_pages: Vec::new(),
            stats: ReclaimStats::default(),
            total_pages,
            used_pages: total_pages,
        }
    }

    pub fn free_page(&mut self, page_id: u32, txn_id: u64, size_bytes: usize) {
        self.free_pages.push(FreePage {
            page_id,
            freed_at_txn: txn_id,
            size_bytes,
        });
        self.stats.pages_freed += 1;
        self.stats.bytes_reclaimed += size_bytes as u64;
        self.used_pages = self.used_pages.saturating_sub(1);
    }

    pub fn allocate_page(&mut self) -> Option<u32> {
        self.free_pages.pop().map(|fp| {
            self.used_pages += 1;
            fp.page_id
        })
    }

    /// 模拟紧凑化（合并空闲页面）
    pub fn compact(&mut self) -> usize {
        self.stats.compactions_run += 1;
        let before = self.free_pages.len();
        // De-duplicate page ids
        self.free_pages.sort_by_key(|p| p.page_id);
        self.free_pages.dedup_by_key(|p| p.page_id);
        let removed = before - self.free_pages.len();
        self.update_fragmentation();
        removed
    }

    fn update_fragmentation(&mut self) {
        if self.total_pages == 0 {
            self.stats.fragmentation_pct = 0.0;
        } else {
            self.stats.fragmentation_pct =
                self.free_pages.len() as f64 / self.total_pages as f64 * 100.0;
        }
    }

    pub fn free_page_count(&self) -> usize {
        self.free_pages.len()
    }

    pub fn utilization(&self) -> f64 {
        if self.total_pages == 0 {
            return 0.0;
        }
        self.used_pages as f64 / self.total_pages as f64
    }

    pub fn stats(&self) -> &ReclaimStats {
        &self.stats
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_columnar_engine_segments() {
        let mut eng = ColumnarEngine::new(1024);
        eng.add_segment(ColumnSegment {
            column_name: "id".into(),
            encoding: ColumnEncoding::DeltaBinary,
            row_count: 1000,
            null_count: 0,
            min_value: 1,
            max_value: 1000,
            compressed_size: 500,
            uncompressed_size: 8000,
        });
        eng.add_segment(ColumnSegment {
            column_name: "name".into(),
            encoding: ColumnEncoding::Dictionary,
            row_count: 1000,
            null_count: 5,
            min_value: 0,
            max_value: 0,
            compressed_size: 2000,
            uncompressed_size: 10000,
        });
        assert_eq!(eng.column_count(), 2);
        let projected = eng.project_scan(&["id"]);
        assert_eq!(projected.len(), 1);
    }

    #[test]
    fn test_columnar_segment_skip() {
        let mut eng = ColumnarEngine::new(512);
        for i in 0..5 {
            eng.add_segment(ColumnSegment {
                column_name: "val".into(),
                encoding: ColumnEncoding::Plain,
                row_count: 100,
                null_count: 0,
                min_value: i * 100,
                max_value: (i + 1) * 100 - 1,
                compressed_size: 400,
                uncompressed_size: 800,
            });
        }
        let matched = eng.segment_skip("val", 150, 250);
        assert_eq!(matched.len(), 2); // segments [100-199] and [200-299]
    }

    #[test]
    fn test_partition_manager() {
        let mut pm = PartitionManager::new();
        let _s = pm.add_scheme(PartitionScheme::Hash {
            column: "user_id".into(),
            num_buckets: 4,
        });
        let p1 = pm.create_partition("p0", 0);
        let p2 = pm.create_partition("p1", 0);
        pm.add_rows(p1, 500, 4096);
        pm.add_rows(p2, 300, 2048);
        assert_eq!(pm.total_rows(), 800);
        assert_eq!(pm.active_partitions(), 2);
        pm.deactivate(p2);
        assert_eq!(pm.active_partitions(), 1);
    }

    #[test]
    fn test_tier_manager_lifecycle() {
        let mut tm = TierManager::new(5, 2, 10000);
        tm.register_block(1, 4096, 1000);
        tm.register_block(2, 4096, 1000);

        // Access block 1 many times → stays hot
        for _ in 0..6 {
            tm.access(1, 5000);
        }
        // Block 2 not accessed → should become cold after rebalance
        tm.rebalance(20000);

        let summary = tm.tier_summary();
        assert!(summary.get(&DataTier::Hot).is_some());
        assert_eq!(tm.block_count(), 2);
    }

    #[test]
    fn test_tier_promotion() {
        let mut tm = TierManager::new(3, 1, 5000);
        tm.register_block(10, 1024, 100);
        tm.rebalance(10000); // should go cold due to age
        let summary = tm.tier_summary();
        assert!(summary.get(&DataTier::Cold).is_some() || summary.get(&DataTier::Frozen).is_some());
        // Now access enough to promote
        for _ in 0..4 {
            tm.access(10, 15000);
        }
        let summary2 = tm.tier_summary();
        assert!(summary2.get(&DataTier::Hot).is_some());
    }

    #[test]
    fn test_space_reclaimer() {
        let mut sr = SpaceReclaimer::new(100);
        sr.free_page(5, 1, 4096);
        sr.free_page(10, 2, 4096);
        sr.free_page(15, 3, 4096);
        assert_eq!(sr.free_page_count(), 3);
        assert_eq!(sr.stats().pages_freed, 3);

        let reused = sr.allocate_page();
        assert!(reused.is_some());
        assert_eq!(sr.free_page_count(), 2);
    }

    #[test]
    fn test_space_reclaimer_compact() {
        let mut sr = SpaceReclaimer::new(50);
        sr.free_page(1, 1, 4096);
        sr.free_page(1, 2, 4096); // duplicate page id
        sr.free_page(2, 3, 4096);
        let removed = sr.compact();
        assert_eq!(removed, 1);
        assert_eq!(sr.free_page_count(), 2);
        assert_eq!(sr.stats().compactions_run, 1);
    }

    #[test]
    fn test_row_group_creation() {
        let mut eng = ColumnarEngine::new(1000);
        eng.create_row_group(vec!["id", "name", "value"], 1000);
        eng.create_row_group(vec!["id", "name", "value"], 500);
        assert_eq!(eng.row_group_count(), 2);
        assert_eq!(eng.total_rows(), 1500);
    }
}
