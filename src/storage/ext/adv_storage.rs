// ── src/storage/ext/adv_storage.rs ──
// R20: 高级存储引擎优化 — 自适应压缩 / 页面预取 / 增量合并 / 存储层监控

use std::collections::{HashMap, VecDeque};

// ═══════════════════════════════════════════════════════════════════════
// 1. AdaptiveCompressor — 自适应压缩策略选择
// ═══════════════════════════════════════════════════════════════════════

/// 压缩算法
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionAlgo {
    None,
    Lz4,
    Snappy,
    Zstd,
    DictZstd,
}

impl CompressionAlgo {
    pub fn ratio_hint(&self) -> f64 {
        match self {
            Self::None => 1.0,
            Self::Lz4 => 0.55,
            Self::Snappy => 0.60,
            Self::Zstd => 0.35,
            Self::DictZstd => 0.28,
        }
    }

    pub fn speed_rank(&self) -> u8 {
        match self {
            Self::None => 5,
            Self::Lz4 => 4,
            Self::Snappy => 4,
            Self::Zstd => 2,
            Self::DictZstd => 1,
        }
    }
}

/// 压缩样本
#[derive(Debug, Clone)]
pub struct CompressionSample {
    pub algo: CompressionAlgo,
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub compress_us: u64,
}

impl CompressionSample {
    pub fn ratio(&self) -> f64 {
        if self.original_bytes == 0 {
            return 1.0;
        }
        self.compressed_bytes as f64 / self.original_bytes as f64
    }
}

/// 自适应压缩器 — 根据数据特征自动选择最优压缩算法
pub struct AdaptiveCompressor {
    samples: HashMap<String, Vec<CompressionSample>>,
    chosen: HashMap<String, CompressionAlgo>,
    max_samples: usize,
    cold_threshold_secs: u64,
}

impl AdaptiveCompressor {
    pub fn new(max_samples: usize, cold_threshold_secs: u64) -> Self {
        Self {
            samples: HashMap::new(),
            chosen: HashMap::new(),
            max_samples,
            cold_threshold_secs,
        }
    }

    pub fn add_sample(&mut self, table: &str, sample: CompressionSample) {
        let v = self.samples.entry(table.to_string()).or_default();
        if v.len() >= self.max_samples {
            v.remove(0);
        }
        v.push(sample);
    }

    /// 根据累积采样自动选择最佳算法
    pub fn select_algo(&mut self, table: &str) -> CompressionAlgo {
        if let Some(cached) = self.chosen.get(table) {
            return *cached;
        }
        let samples = match self.samples.get(table) {
            Some(s) if !s.is_empty() => s,
            _ => return CompressionAlgo::Lz4, // default
        };

        // 按 ratio 加权排序，ratio 越小越好
        let mut algo_stats: HashMap<CompressionAlgo, (f64, u64, usize)> = HashMap::new();
        for s in samples {
            let e = algo_stats.entry(s.algo).or_insert((0.0, 0, 0));
            e.0 += s.ratio();
            e.1 += s.compress_us;
            e.2 += 1;
        }

        let best = algo_stats
            .iter()
            .map(|(algo, (ratio_sum, time_sum, count))| {
                let avg_ratio = ratio_sum / *count as f64;
                let avg_time = *time_sum as f64 / *count as f64;
                // 综合得分：ratio 权重 0.7, 速度权重 0.3
                let score = avg_ratio * 0.7 + (avg_time / 1000.0).min(1.0) * 0.3;
                (*algo, score)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(algo, _)| algo)
            .unwrap_or(CompressionAlgo::Lz4);

        self.chosen.insert(table.to_string(), best);
        best
    }

    pub fn cold_threshold(&self) -> u64 {
        self.cold_threshold_secs
    }

    pub fn invalidate(&mut self, table: &str) {
        self.chosen.remove(table);
    }

    pub fn table_count(&self) -> usize {
        self.samples.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. PagePrefetcher — 页面预取策略
// ═══════════════════════════════════════════════════════════════════════

/// 预取类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchMode {
    Sequential,  // 顺序预取
    Stride,      // 步幅预取
    Adaptive,    // 自适应
}

/// 预取请求
#[derive(Debug, Clone)]
pub struct PrefetchRequest {
    pub page_id: u32,
    pub priority: u8,
    pub mode: PrefetchMode,
}

/// 页面预取器 — 基于访问模式预测下一批页面
pub struct PagePrefetcher {
    history: VecDeque<u32>,
    max_history: usize,
    lookahead: usize,
    mode: PrefetchMode,
    hits: u64,
    misses: u64,
}

impl PagePrefetcher {
    pub fn new(lookahead: usize) -> Self {
        Self {
            history: VecDeque::new(),
            max_history: 256,
            lookahead,
            mode: PrefetchMode::Sequential,
            hits: 0,
            misses: 0,
        }
    }

    pub fn record_access(&mut self, page_id: u32) {
        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(page_id);
        self.detect_pattern();
    }

    fn detect_pattern(&mut self) {
        if self.history.len() < 4 {
            return;
        }
        let len = self.history.len();
        let d1 = self.history[len - 1] as i64 - self.history[len - 2] as i64;
        let d2 = self.history[len - 2] as i64 - self.history[len - 3] as i64;
        let d3 = self.history[len - 3] as i64 - self.history[len - 4] as i64;

        if d1 == 1 && d2 == 1 && d3 == 1 {
            self.mode = PrefetchMode::Sequential;
        } else if d1 == d2 && d2 == d3 && d1 != 0 {
            self.mode = PrefetchMode::Stride;
        } else {
            self.mode = PrefetchMode::Adaptive;
        }
    }

    /// 生成预取请求列表
    pub fn generate_prefetch(&self) -> Vec<PrefetchRequest> {
        let last = match self.history.back() {
            Some(p) => *p,
            None => return vec![],
        };

        match self.mode {
            PrefetchMode::Sequential => (1..=self.lookahead as u32)
                .map(|i| PrefetchRequest {
                    page_id: last + i,
                    priority: (self.lookahead as u8).saturating_sub(i as u8),
                    mode: self.mode,
                })
                .collect(),
            PrefetchMode::Stride => {
                if self.history.len() < 2 {
                    return vec![];
                }
                let len = self.history.len();
                let stride = self.history[len - 1] as i64 - self.history[len - 2] as i64;
                (1..=self.lookahead)
                    .filter_map(|i| {
                        let predicted = last as i64 + stride * i as i64;
                        if predicted > 0 {
                            Some(PrefetchRequest {
                                page_id: predicted as u32,
                                priority: (self.lookahead as u8).saturating_sub(i as u8),
                                mode: self.mode,
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            PrefetchMode::Adaptive => {
                // 简单策略：预取前后各一半
                let half = self.lookahead / 2;
                let mut reqs = Vec::new();
                for i in 1..=half as u32 {
                    reqs.push(PrefetchRequest {
                        page_id: last + i,
                        priority: 2,
                        mode: self.mode,
                    });
                    if last > i {
                        reqs.push(PrefetchRequest {
                            page_id: last - i,
                            priority: 1,
                            mode: self.mode,
                        });
                    }
                }
                reqs
            }
        }
    }

    pub fn record_hit(&mut self) {
        self.hits += 1;
    }

    pub fn record_miss(&mut self) {
        self.misses += 1;
    }

    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f64 / total as f64
    }

    pub fn current_mode(&self) -> PrefetchMode {
        self.mode
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. IncrementalMerger — 增量合并引擎
// ═══════════════════════════════════════════════════════════════════════

/// 合并段
#[derive(Debug, Clone)]
pub struct MergeSegment {
    pub id: u64,
    pub level: u8,
    pub key_count: usize,
    pub size_bytes: usize,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
}

impl MergeSegment {
    pub fn overlaps(&self, other: &MergeSegment) -> bool {
        self.min_key <= other.max_key && other.min_key <= self.max_key
    }
}

/// 合并策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    SizeTiered,   // 大小分层
    Leveled,      // 分级
    Hybrid,       // 混合
}

/// 增量合并器 — 管理 LSM 风格的增量合并
pub struct IncrementalMerger {
    segments: Vec<MergeSegment>,
    strategy: MergeStrategy,
    level_sizes: Vec<usize>,
    next_id: u64,
    merges_done: u64,
    bytes_merged: u64,
}

impl IncrementalMerger {
    pub fn new(strategy: MergeStrategy) -> Self {
        Self {
            segments: Vec::new(),
            strategy,
            level_sizes: vec![4, 10, 10, 10], // L0=4, L1-L3=10
            next_id: 1,
            merges_done: 0,
            bytes_merged: 0,
        }
    }

    pub fn add_segment(&mut self, level: u8, key_count: usize, size_bytes: usize, min_key: Vec<u8>, max_key: Vec<u8>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.segments.push(MergeSegment {
            id,
            level,
            key_count,
            size_bytes,
            min_key,
            max_key,
        });
        id
    }

    /// 选择待合并的段
    pub fn pick_merge_candidates(&self) -> Vec<Vec<u64>> {
        let mut groups: Vec<Vec<u64>> = Vec::new();

        match self.strategy {
            MergeStrategy::SizeTiered => {
                // 同级别、大小相近的段分组
                let mut by_level: HashMap<u8, Vec<&MergeSegment>> = HashMap::new();
                for seg in &self.segments {
                    by_level.entry(seg.level).or_default().push(seg);
                }
                for (level, segs) in &by_level {
                    let max = self.level_sizes.get(*level as usize).copied().unwrap_or(10);
                    if segs.len() >= max {
                        groups.push(segs.iter().map(|s| s.id).collect());
                    }
                }
            }
            MergeStrategy::Leveled => {
                // 选择 L0 中所有段 + L1 中重叠段
                let l0: Vec<&MergeSegment> = self.segments.iter().filter(|s| s.level == 0).collect();
                let max_l0 = self.level_sizes.first().copied().unwrap_or(4);
                if l0.len() >= max_l0 {
                    let mut ids: Vec<u64> = l0.iter().map(|s| s.id).collect();
                    for seg in &self.segments {
                        if seg.level == 1 && l0.iter().any(|s| s.overlaps(seg)) {
                            ids.push(seg.id);
                        }
                    }
                    groups.push(ids);
                }
            }
            MergeStrategy::Hybrid => {
                // L0 用 SizeTiered, L1+ 用 Leveled
                let l0: Vec<&MergeSegment> = self.segments.iter().filter(|s| s.level == 0).collect();
                let max_l0 = self.level_sizes.first().copied().unwrap_or(4);
                if l0.len() >= max_l0 {
                    groups.push(l0.iter().map(|s| s.id).collect());
                }
            }
        }
        groups
    }

    /// 执行合并（标记完成）
    pub fn complete_merge(&mut self, merged_ids: &[u64], result: MergeSegment) {
        let total_bytes: u64 = self.segments.iter()
            .filter(|s| merged_ids.contains(&s.id))
            .map(|s| s.size_bytes as u64)
            .sum();
        self.segments.retain(|s| !merged_ids.contains(&s.id));
        self.segments.push(result);
        self.merges_done += 1;
        self.bytes_merged += total_bytes;
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn merges_done(&self) -> u64 {
        self.merges_done
    }

    pub fn bytes_merged(&self) -> u64 {
        self.bytes_merged
    }

    pub fn segments_at_level(&self, level: u8) -> usize {
        self.segments.iter().filter(|s| s.level == level).count()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. StorageLayerMonitor — 存储层综合监控
// ═══════════════════════════════════════════════════════════════════════

/// IO 操作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoOp {
    Read,
    Write,
    Fsync,
    Seek,
}

/// IO 统计
#[derive(Debug, Clone, Default)]
pub struct IoStats {
    pub count: u64,
    pub total_bytes: u64,
    pub total_us: u64,
    pub max_us: u64,
}

impl IoStats {
    pub fn record(&mut self, bytes: u64, duration_us: u64) {
        self.count += 1;
        self.total_bytes += bytes;
        self.total_us += duration_us;
        if duration_us > self.max_us {
            self.max_us = duration_us;
        }
    }

    pub fn avg_us(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.total_us as f64 / self.count as f64 }
    }

    pub fn throughput_mbps(&self, elapsed_secs: f64) -> f64 {
        if elapsed_secs <= 0.0 { return 0.0; }
        (self.total_bytes as f64) / (1024.0 * 1024.0) / elapsed_secs
    }
}

/// 存储层告警
#[derive(Debug, Clone)]
pub struct StorageAlert {
    pub component: String,
    pub severity: AlertSeverity,
    pub message: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

/// 存储层综合监控
pub struct StorageLayerMonitor {
    io_stats: HashMap<IoOp, IoStats>,
    alerts: VecDeque<StorageAlert>,
    max_alerts: usize,
    page_cache_hits: u64,
    page_cache_misses: u64,
    wal_writes: u64,
    wal_bytes: u64,
    checkpoint_count: u64,
}

impl StorageLayerMonitor {
    pub fn new(max_alerts: usize) -> Self {
        Self {
            io_stats: HashMap::new(),
            alerts: VecDeque::new(),
            max_alerts,
            page_cache_hits: 0,
            page_cache_misses: 0,
            wal_writes: 0,
            wal_bytes: 0,
            checkpoint_count: 0,
        }
    }

    pub fn record_io(&mut self, op: IoOp, bytes: u64, duration_us: u64) {
        self.io_stats.entry(op).or_default().record(bytes, duration_us);
    }

    pub fn get_io_stats(&self, op: IoOp) -> Option<&IoStats> {
        self.io_stats.get(&op)
    }

    pub fn record_page_cache_hit(&mut self) {
        self.page_cache_hits += 1;
    }

    pub fn record_page_cache_miss(&mut self) {
        self.page_cache_misses += 1;
    }

    pub fn page_cache_hit_rate(&self) -> f64 {
        let total = self.page_cache_hits + self.page_cache_misses;
        if total == 0 { 0.0 } else { self.page_cache_hits as f64 / total as f64 }
    }

    pub fn record_wal_write(&mut self, bytes: u64) {
        self.wal_writes += 1;
        self.wal_bytes += bytes;
    }

    pub fn record_checkpoint(&mut self) {
        self.checkpoint_count += 1;
    }

    pub fn add_alert(&mut self, component: &str, severity: AlertSeverity, message: &str) {
        if self.alerts.len() >= self.max_alerts {
            self.alerts.pop_front();
        }
        self.alerts.push_back(StorageAlert {
            component: component.to_string(),
            severity,
            message: message.to_string(),
            timestamp_ms: 0,
        });
    }

    pub fn alerts(&self) -> &VecDeque<StorageAlert> {
        &self.alerts
    }

    pub fn critical_alert_count(&self) -> usize {
        self.alerts.iter().filter(|a| a.severity == AlertSeverity::Critical).count()
    }

    pub fn summary(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("page_cache_hit_rate".into(), format!("{:.2}%", self.page_cache_hit_rate() * 100.0));
        m.insert("wal_writes".into(), self.wal_writes.to_string());
        m.insert("wal_bytes".into(), self.wal_bytes.to_string());
        m.insert("checkpoints".into(), self.checkpoint_count.to_string());
        m.insert("alerts".into(), self.alerts.len().to_string());
        for (op, stats) in &self.io_stats {
            m.insert(format!("{:?}_count", op), stats.count.to_string());
            m.insert(format!("{:?}_avg_us", op), format!("{:.1}", stats.avg_us()));
        }
        m
    }

    pub fn wal_writes(&self) -> u64 {
        self.wal_writes
    }

    pub fn checkpoint_count(&self) -> u64 {
        self.checkpoint_count
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_compressor_select() {
        let mut ac = AdaptiveCompressor::new(100, 3600);
        ac.add_sample("t1", CompressionSample {
            algo: CompressionAlgo::Lz4,
            original_bytes: 1000,
            compressed_bytes: 500,
            compress_us: 10,
        });
        ac.add_sample("t1", CompressionSample {
            algo: CompressionAlgo::Zstd,
            original_bytes: 1000,
            compressed_bytes: 300,
            compress_us: 50,
        });
        let algo = ac.select_algo("t1");
        // Zstd has better ratio (0.3 vs 0.5)
        assert_eq!(algo, CompressionAlgo::Zstd);
        assert_eq!(ac.table_count(), 1);
    }

    #[test]
    fn test_compressor_invalidate() {
        let mut ac = AdaptiveCompressor::new(50, 1800);
        ac.add_sample("t1", CompressionSample {
            algo: CompressionAlgo::Snappy,
            original_bytes: 1000,
            compressed_bytes: 600,
            compress_us: 5,
        });
        let _ = ac.select_algo("t1");
        ac.invalidate("t1");
        // Re-select after invalidation
        let algo = ac.select_algo("t1");
        assert_eq!(algo, CompressionAlgo::Snappy);
    }

    #[test]
    fn test_compression_algo_props() {
        assert!(CompressionAlgo::Zstd.ratio_hint() < CompressionAlgo::Lz4.ratio_hint());
        assert!(CompressionAlgo::Lz4.speed_rank() > CompressionAlgo::Zstd.speed_rank());
        assert_eq!(CompressionAlgo::None.ratio_hint(), 1.0);
    }

    #[test]
    fn test_page_prefetcher_sequential() {
        let mut pf = PagePrefetcher::new(4);
        for i in 1..=6 {
            pf.record_access(i);
        }
        assert_eq!(pf.current_mode(), PrefetchMode::Sequential);
        let reqs = pf.generate_prefetch();
        assert_eq!(reqs.len(), 4);
        assert_eq!(reqs[0].page_id, 7);
        assert_eq!(reqs[3].page_id, 10);
    }

    #[test]
    fn test_page_prefetcher_stride() {
        let mut pf = PagePrefetcher::new(3);
        for &p in &[2, 4, 6, 8] {
            pf.record_access(p);
        }
        assert_eq!(pf.current_mode(), PrefetchMode::Stride);
        let reqs = pf.generate_prefetch();
        assert!(!reqs.is_empty());
        assert_eq!(reqs[0].page_id, 10);
    }

    #[test]
    fn test_prefetcher_hit_rate() {
        let mut pf = PagePrefetcher::new(4);
        pf.record_hit();
        pf.record_hit();
        pf.record_miss();
        assert!((pf.hit_rate() - 0.6667).abs() < 0.01);
    }

    #[test]
    fn test_incremental_merger_size_tiered() {
        let mut m = IncrementalMerger::new(MergeStrategy::SizeTiered);
        for i in 0..5 {
            m.add_segment(0, 100, 1000, vec![i], vec![i + 10]);
        }
        assert_eq!(m.segment_count(), 5);
        let candidates = m.pick_merge_candidates();
        // L0 has 5 >= threshold 4
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].len(), 5);
    }

    #[test]
    fn test_incremental_merger_complete() {
        let mut m = IncrementalMerger::new(MergeStrategy::SizeTiered);
        let id1 = m.add_segment(0, 100, 500, vec![0], vec![50]);
        let id2 = m.add_segment(0, 100, 500, vec![51], vec![100]);
        m.complete_merge(&[id1, id2], MergeSegment {
            id: 999,
            level: 1,
            key_count: 200,
            size_bytes: 900,
            min_key: vec![0],
            max_key: vec![100],
        });
        assert_eq!(m.segment_count(), 1);
        assert_eq!(m.merges_done(), 1);
        assert_eq!(m.bytes_merged(), 1000);
    }

    #[test]
    fn test_storage_monitor_io() {
        let mut mon = StorageLayerMonitor::new(100);
        mon.record_io(IoOp::Read, 4096, 50);
        mon.record_io(IoOp::Read, 4096, 100);
        mon.record_io(IoOp::Write, 8192, 200);
        let rs = mon.get_io_stats(IoOp::Read).unwrap();
        assert_eq!(rs.count, 2);
        assert!((rs.avg_us() - 75.0).abs() < 0.1);
        assert_eq!(rs.max_us, 100);
    }

    #[test]
    fn test_storage_monitor_cache_rate() {
        let mut mon = StorageLayerMonitor::new(50);
        for _ in 0..8 {
            mon.record_page_cache_hit();
        }
        for _ in 0..2 {
            mon.record_page_cache_miss();
        }
        assert!((mon.page_cache_hit_rate() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_storage_monitor_alerts() {
        let mut mon = StorageLayerMonitor::new(3);
        mon.add_alert("wal", AlertSeverity::Warning, "WAL size large");
        mon.add_alert("pager", AlertSeverity::Critical, "Page corruption");
        mon.add_alert("buffer", AlertSeverity::Info, "Eviction spike");
        mon.add_alert("io", AlertSeverity::Critical, "Slow fsync");
        // max_alerts=3, so first one evicted
        assert_eq!(mon.alerts().len(), 3);
        assert_eq!(mon.critical_alert_count(), 2);
    }

    #[test]
    fn test_storage_monitor_summary() {
        let mut mon = StorageLayerMonitor::new(10);
        mon.record_wal_write(4096);
        mon.record_checkpoint();
        let s = mon.summary();
        assert_eq!(s.get("wal_writes").unwrap(), "1");
        assert_eq!(s.get("checkpoints").unwrap(), "1");
    }

    #[test]
    fn test_merge_segment_overlaps() {
        let s1 = MergeSegment { id: 1, level: 0, key_count: 10, size_bytes: 100, min_key: vec![0], max_key: vec![50] };
        let s2 = MergeSegment { id: 2, level: 0, key_count: 10, size_bytes: 100, min_key: vec![40], max_key: vec![80] };
        let s3 = MergeSegment { id: 3, level: 0, key_count: 10, size_bytes: 100, min_key: vec![60], max_key: vec![90] };
        assert!(s1.overlaps(&s2));
        assert!(!s1.overlaps(&s3));
    }
}
