// ── src/vector/vector_advanced.rs ──
// R21: 向量搜索进阶 — 多向量索引 / 混合搜索 / 量化压缩 / 批量导入优化

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
// 1. MultiVectorIndex — 多向量索引管理
// ═══════════════════════════════════════════════════════════════════════

/// 向量索引配置
#[derive(Debug, Clone)]
pub struct VectorIndexConfig {
    pub name: String,
    pub dim: usize,
    pub metric: DistanceMetric,
    pub ef_construction: usize,
    pub max_neighbors: usize,
}

/// 距离度量
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Euclidean,
    Cosine,
    InnerProduct,
    Manhattan,
}

/// 索引统计
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub vector_count: usize,
    pub memory_bytes: usize,
    pub search_count: u64,
    pub avg_search_us: f64,
}

/// 多向量索引管理器
pub struct MultiVectorIndex {
    indexes: HashMap<String, VectorIndexConfig>,
    stats: HashMap<String, IndexStats>,
    vectors: HashMap<String, Vec<(u64, Vec<f32>)>>, // name -> [(id, vector)]
}

impl MultiVectorIndex {
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
            stats: HashMap::new(),
            vectors: HashMap::new(),
        }
    }

    pub fn create_index(&mut self, config: VectorIndexConfig) -> bool {
        if self.indexes.contains_key(&config.name) {
            return false;
        }
        let name = config.name.clone();
        self.indexes.insert(name.clone(), config);
        self.stats.insert(name.clone(), IndexStats::default());
        self.vectors.insert(name, Vec::new());
        true
    }

    pub fn insert(&mut self, index_name: &str, id: u64, vector: Vec<f32>) -> bool {
        let config = match self.indexes.get(index_name) {
            Some(c) => c,
            None => return false,
        };
        if vector.len() != config.dim {
            return false;
        }
        if let Some(vecs) = self.vectors.get_mut(index_name) {
            vecs.push((id, vector));
            if let Some(stats) = self.stats.get_mut(index_name) {
                stats.vector_count += 1;
                stats.memory_bytes += config.dim * 4; // f32 = 4 bytes
            }
            true
        } else {
            false
        }
    }

    /// 暴力搜索（模拟 — 实际用 HNSW）
    pub fn search(&mut self, index_name: &str, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        let config = match self.indexes.get(index_name) {
            Some(c) => c,
            None => return vec![],
        };
        let vecs = match self.vectors.get(index_name) {
            Some(v) => v,
            None => return vec![],
        };

        if let Some(stats) = self.stats.get_mut(index_name) {
            stats.search_count += 1;
        }

        let mut distances: Vec<(u64, f32)> = vecs
            .iter()
            .map(|(id, v)| {
                let dist = match config.metric {
                    DistanceMetric::Euclidean => query
                        .iter()
                        .zip(v)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f32>()
                        .sqrt(),
                    DistanceMetric::Cosine => {
                        let dot: f32 = query.iter().zip(v).map(|(a, b)| a * b).sum();
                        let norm_a: f32 = query.iter().map(|a| a * a).sum::<f32>().sqrt();
                        let norm_b: f32 = v.iter().map(|b| b * b).sum::<f32>().sqrt();
                        if norm_a * norm_b == 0.0 {
                            1.0
                        } else {
                            1.0 - dot / (norm_a * norm_b)
                        }
                    }
                    DistanceMetric::InnerProduct => {
                        -query.iter().zip(v).map(|(a, b)| a * b).sum::<f32>()
                    }
                    DistanceMetric::Manhattan => {
                        query.iter().zip(v).map(|(a, b)| (a - b).abs()).sum()
                    }
                };
                (*id, dist)
            })
            .collect();

        distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        distances.truncate(k);
        distances
    }

    pub fn index_count(&self) -> usize {
        self.indexes.len()
    }

    pub fn get_stats(&self, index_name: &str) -> Option<&IndexStats> {
        self.stats.get(index_name)
    }

    pub fn drop_index(&mut self, name: &str) -> bool {
        self.indexes.remove(name).is_some()
            && self.stats.remove(name).is_some()
            && self.vectors.remove(name).is_some()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. HybridSearcher — 混合搜索（向量 + 关键词）
// ═══════════════════════════════════════════════════════════════════════

/// 搜索结果
#[derive(Debug, Clone)]
pub struct HybridResult {
    pub doc_id: u64,
    pub vector_score: f32,
    pub keyword_score: f32,
    pub combined_score: f32,
}

/// 混合搜索器
pub struct HybridSearcher {
    alpha: f32, // vector weight (1-alpha = keyword weight)
    results_cache: Vec<HybridResult>,
    searches: u64,
}

impl HybridSearcher {
    pub fn new(alpha: f32) -> Self {
        Self {
            alpha: alpha.clamp(0.0, 1.0),
            results_cache: Vec::new(),
            searches: 0,
        }
    }

    /// 合并向量搜索和关键词搜索结果
    pub fn merge(
        &mut self,
        vector_results: &[(u64, f32)],
        keyword_results: &[(u64, f32)],
        k: usize,
    ) -> Vec<HybridResult> {
        self.searches += 1;
        let mut scores: HashMap<u64, (f32, f32)> = HashMap::new();

        // Normalize vector scores (lower distance = higher score)
        let max_vdist = vector_results
            .iter()
            .map(|(_, d)| *d)
            .fold(f32::MIN, f32::max)
            .max(1.0);
        for &(id, dist) in vector_results {
            let score = 1.0 - (dist / max_vdist);
            scores.entry(id).or_insert((0.0, 0.0)).0 = score;
        }

        // Keyword scores (already normalized, higher = better)
        for &(id, score) in keyword_results {
            scores.entry(id).or_insert((0.0, 0.0)).1 = score;
        }

        let mut results: Vec<HybridResult> = scores
            .into_iter()
            .map(|(id, (vs, ks))| {
                let combined = self.alpha * vs + (1.0 - self.alpha) * ks;
                HybridResult {
                    doc_id: id,
                    vector_score: vs,
                    keyword_score: ks,
                    combined_score: combined,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);
        self.results_cache = results.clone();
        results
    }

    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha.clamp(0.0, 1.0);
    }

    pub fn search_count(&self) -> u64 {
        self.searches
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. QuantizedCompressor — 向量量化压缩
// ═══════════════════════════════════════════════════════════════════════

/// 量化方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizeMethod {
    Scalar8,  // 8-bit scalar quantization
    Scalar4,  // 4-bit scalar quantization
    ProductQ, // Product quantization
}

/// 量化后的向量
#[derive(Debug, Clone)]
pub struct QuantizedVector {
    pub id: u64,
    pub data: Vec<u8>,
    pub method: QuantizeMethod,
    pub original_dim: usize,
}

/// 向量量化压缩器
pub struct QuantizedCompressor {
    method: QuantizeMethod,
    min_vals: Vec<f32>,
    max_vals: Vec<f32>,
    compressed_count: u64,
}

impl QuantizedCompressor {
    pub fn new(method: QuantizeMethod) -> Self {
        Self {
            method,
            min_vals: Vec::new(),
            max_vals: Vec::new(),
            compressed_count: 0,
        }
    }

    /// 训练量化参数（记录 min/max）
    pub fn train(&mut self, vectors: &[Vec<f32>]) {
        if vectors.is_empty() {
            return;
        }
        let dim = vectors[0].len();
        self.min_vals = vec![f32::MAX; dim];
        self.max_vals = vec![f32::MIN; dim];

        for v in vectors {
            for (i, &val) in v.iter().enumerate() {
                if val < self.min_vals[i] {
                    self.min_vals[i] = val;
                }
                if val > self.max_vals[i] {
                    self.max_vals[i] = val;
                }
            }
        }
    }

    /// 量化单个向量
    pub fn compress(&mut self, id: u64, vector: &[f32]) -> QuantizedVector {
        self.compressed_count += 1;
        let data: Vec<u8> = match self.method {
            QuantizeMethod::Scalar8 => vector
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    let min = self.min_vals.get(i).copied().unwrap_or(0.0);
                    let max = self.max_vals.get(i).copied().unwrap_or(1.0);
                    let range = (max - min).max(1e-10);
                    ((v - min) / range * 255.0).clamp(0.0, 255.0) as u8
                })
                .collect(),
            QuantizeMethod::Scalar4 => {
                // Pack two 4-bit values per byte
                vector
                    .chunks(2)
                    .enumerate()
                    .map(|(_, chunk)| {
                        let high = ((chunk[0].clamp(0.0, 1.0) * 15.0) as u8) << 4;
                        let low = if chunk.len() > 1 {
                            (chunk[1].clamp(0.0, 1.0) * 15.0) as u8
                        } else {
                            0
                        };
                        high | low
                    })
                    .collect()
            }
            QuantizeMethod::ProductQ => {
                // Simplified: just 8-bit for now
                vector
                    .iter()
                    .map(|&v| (v.clamp(0.0, 1.0) * 255.0) as u8)
                    .collect()
            }
        };

        QuantizedVector {
            id,
            data,
            method: self.method,
            original_dim: vector.len(),
        }
    }

    pub fn compression_ratio(&self, original_dim: usize) -> f64 {
        let original_bytes = original_dim * 4; // f32
        let compressed_bytes = match self.method {
            QuantizeMethod::Scalar8 => original_dim,
            QuantizeMethod::Scalar4 => (original_dim + 1) / 2,
            QuantizeMethod::ProductQ => original_dim,
        };
        compressed_bytes as f64 / original_bytes as f64
    }

    pub fn compressed_count(&self) -> u64 {
        self.compressed_count
    }

    pub fn is_trained(&self) -> bool {
        !self.min_vals.is_empty()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. BatchImporter — 批量向量导入
// ═══════════════════════════════════════════════════════════════════════

/// 导入状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    Pending,
    InProgress,
    Complete,
    Failed,
}

/// 批量导入任务
#[derive(Debug, Clone)]
pub struct ImportJob {
    pub job_id: u64,
    pub index_name: String,
    pub total_vectors: usize,
    pub imported: usize,
    pub status: ImportStatus,
    pub errors: usize,
}

/// 批量导入器
pub struct BatchImporter {
    jobs: Vec<ImportJob>,
    next_job_id: u64,
    batch_size: usize,
    total_imported: u64,
}

impl BatchImporter {
    pub fn new(batch_size: usize) -> Self {
        Self {
            jobs: Vec::new(),
            next_job_id: 1,
            batch_size,
            total_imported: 0,
        }
    }

    pub fn create_job(&mut self, index_name: &str, total: usize) -> u64 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(ImportJob {
            job_id: id,
            index_name: index_name.to_string(),
            total_vectors: total,
            imported: 0,
            status: ImportStatus::Pending,
            errors: 0,
        });
        id
    }

    /// 模拟导入一批
    pub fn import_batch(&mut self, job_id: u64, count: usize) -> bool {
        let job = match self.jobs.iter_mut().find(|j| j.job_id == job_id) {
            Some(j) => j,
            None => return false,
        };
        if job.status == ImportStatus::Complete || job.status == ImportStatus::Failed {
            return false;
        }
        job.status = ImportStatus::InProgress;
        let import_count = count.min(job.total_vectors - job.imported);
        job.imported += import_count;
        self.total_imported += import_count as u64;

        if job.imported >= job.total_vectors {
            job.status = ImportStatus::Complete;
        }
        true
    }

    pub fn job_progress(&self, job_id: u64) -> Option<(usize, usize)> {
        self.jobs
            .iter()
            .find(|j| j.job_id == job_id)
            .map(|j| (j.imported, j.total_vectors))
    }

    pub fn job_status(&self, job_id: u64) -> Option<ImportStatus> {
        self.jobs
            .iter()
            .find(|j| j.job_id == job_id)
            .map(|j| j.status)
    }

    pub fn active_jobs(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.status == ImportStatus::InProgress)
            .count()
    }

    pub fn total_imported(&self) -> u64 {
        self.total_imported
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multi_vector_index_create_search() {
        let mut mvi = MultiVectorIndex::new();
        mvi.create_index(VectorIndexConfig {
            name: "embeddings".into(),
            dim: 3,
            metric: DistanceMetric::Euclidean,
            ef_construction: 200,
            max_neighbors: 16,
        });

        mvi.insert("embeddings", 1, vec![1.0, 0.0, 0.0]);
        mvi.insert("embeddings", 2, vec![0.0, 1.0, 0.0]);
        mvi.insert("embeddings", 3, vec![0.0, 0.0, 1.0]);

        let results = mvi.search("embeddings", &[1.0, 0.1, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1); // closest
    }

    #[test]
    fn test_multi_vector_cosine() {
        let mut mvi = MultiVectorIndex::new();
        mvi.create_index(VectorIndexConfig {
            name: "cos".into(),
            dim: 2,
            metric: DistanceMetric::Cosine,
            ef_construction: 100,
            max_neighbors: 8,
        });
        mvi.insert("cos", 1, vec![1.0, 0.0]);
        mvi.insert("cos", 2, vec![0.707, 0.707]);
        let results = mvi.search("cos", &[1.0, 0.0], 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_multi_vector_dimension_mismatch() {
        let mut mvi = MultiVectorIndex::new();
        mvi.create_index(VectorIndexConfig {
            name: "v".into(),
            dim: 4,
            metric: DistanceMetric::Euclidean,
            ef_construction: 100,
            max_neighbors: 8,
        });
        assert!(!mvi.insert("v", 1, vec![1.0, 2.0])); // dim=2 != 4
    }

    #[test]
    fn test_hybrid_search_merge() {
        let mut hs = HybridSearcher::new(0.6);
        let vec_results = vec![(1, 0.1f32), (2, 0.5), (3, 0.9)];
        let kw_results = vec![(2, 0.8f32), (3, 0.9), (4, 0.7)];

        let results = hs.merge(&vec_results, &kw_results, 3);
        assert_eq!(results.len(), 3);
        // results sorted by combined score desc
        assert!(results[0].combined_score >= results[1].combined_score);
    }

    #[test]
    fn test_hybrid_search_alpha() {
        let mut hs = HybridSearcher::new(1.0);
        assert_eq!(hs.alpha(), 1.0);
        hs.set_alpha(0.3);
        assert!((hs.alpha() - 0.3).abs() < 0.001);
    }

    #[test]
    fn test_quantized_scalar8() {
        let mut qc = QuantizedCompressor::new(QuantizeMethod::Scalar8);
        qc.train(&[vec![0.0, 0.5, 1.0], vec![0.2, 0.8, 0.3]]);
        assert!(qc.is_trained());

        let qv = qc.compress(1, &[0.5, 0.5, 0.5]);
        assert_eq!(qv.original_dim, 3);
        assert_eq!(qv.data.len(), 3); // 1 byte per dim
        assert!(qc.compression_ratio(3) < 0.5); // 3 bytes vs 12 bytes
    }

    #[test]
    fn test_quantized_scalar4() {
        let mut qc = QuantizedCompressor::new(QuantizeMethod::Scalar4);
        qc.train(&[vec![0.0; 4]]);
        let qv = qc.compress(1, &[0.5, 0.5, 0.5, 0.5]);
        assert_eq!(qv.data.len(), 2); // 4 dims → 2 bytes (4-bit packing)
    }

    #[test]
    fn test_batch_importer_lifecycle() {
        let mut imp = BatchImporter::new(1000);
        let jid = imp.create_job("main_index", 5000);

        imp.import_batch(jid, 1000);
        assert_eq!(imp.job_progress(jid), Some((1000, 5000)));
        assert_eq!(imp.job_status(jid), Some(ImportStatus::InProgress));

        imp.import_batch(jid, 4000);
        assert_eq!(imp.job_status(jid), Some(ImportStatus::Complete));
        assert_eq!(imp.total_imported(), 5000);
    }

    #[test]
    fn test_batch_importer_multiple_jobs() {
        let mut imp = BatchImporter::new(500);
        let j1 = imp.create_job("idx1", 100);
        let j2 = imp.create_job("idx2", 200);
        imp.import_batch(j1, 100);
        imp.import_batch(j2, 50);
        assert_eq!(imp.active_jobs(), 1); // j2 still in progress
        assert_eq!(imp.total_imported(), 150);
    }

    #[test]
    fn test_drop_index() {
        let mut mvi = MultiVectorIndex::new();
        mvi.create_index(VectorIndexConfig {
            name: "temp".into(),
            dim: 2,
            metric: DistanceMetric::Euclidean,
            ef_construction: 50,
            max_neighbors: 4,
        });
        assert_eq!(mvi.index_count(), 1);
        assert!(mvi.drop_index("temp"));
        assert_eq!(mvi.index_count(), 0);
    }
}
