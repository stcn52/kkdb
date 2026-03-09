/// Vector search module — management interface for KKDB's in-process HNSW engine.
///
/// Mirrors the role of `fulltext/mod.rs` in the FTS subsystem.
///
/// # Module layout
///
/// ```text
/// src/vector/
///   mod.rs       - VectorIndex, VectorIndexRegistry (this file)
///   hnsw.rs      - HnswGraph: insert / search / lazy_delete / rebuild
///   distance.rs  - DistanceMetric, cosine_similarity, l2_distance
///   index.rs     - B-Tree key/value encoding helpers
/// ```
pub mod distance;
pub mod hnsw;
pub mod index;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::vector::distance::DistanceMetric;
use crate::vector::hnsw::HnswGraph;

// ─── VectorIndex ─────────────────────────────────────────────────────────────

/// Metadata + in-memory HNSW graph for one vector index.
///
/// Created on `CREATE VECTOR INDEX` DDL and stored in `Schema.vector_indexes`.
#[derive(Clone)]
pub struct VectorIndex {
    /// Index name (lowercase).
    pub name: String,
    /// Target table name (lowercase).
    pub table: String,
    /// Target column name (lowercase).
    pub column: String,
    /// Column offset within the table's schema (used by exec_dml).
    pub col_idx: usize,
    /// Expected vector dimension; enforced on write.
    pub dim: u32,
    /// Distance metric.
    pub distance: DistanceMetric,
    /// Numeric ID used to namespace B-Tree keys (allocated by schema).
    pub index_id: u32,
    /// The live HNSW graph (Arc<RwLock<…>> for multi-reader single-writer access).
    pub hnsw: Arc<RwLock<HnswGraph>>,
}

impl std::fmt::Debug for VectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "VectorIndex {{ name: {:?}, table: {:?}, column: {:?}, dim: {}, distance: {:?}, index_id: {} }}",
            self.name, self.table, self.column, self.dim, self.distance, self.index_id
        )
    }
}

impl VectorIndex {
    /// Create a new empty vector index with a fresh HNSW graph.
    pub fn new(
        name: String,
        table: String,
        column: String,
        col_idx: usize,
        dim: u32,
        distance: DistanceMetric,
        index_id: u32,
    ) -> Self {
        let graph = HnswGraph::new(hnsw::DEFAULT_M, hnsw::DEFAULT_EF_CONSTRUCTION, distance);
        Self {
            name,
            table,
            column,
            col_idx,
            dim,
            distance,
            index_id,
            hnsw: Arc::new(RwLock::new(graph)),
        }
    }

    /// Insert a vector for the given rowid into the in-memory HNSW graph.
    ///
    /// Returns an error if the dimension doesn't match the index's declared `dim`.
    pub fn insert_vec(&self, rowid: u64, vec: Vec<f32>) -> crate::error::Result<()> {
        if vec.len() as u32 != self.dim {
            return Err(crate::error::KkdbError::RuntimeError(format!(
                "vector index '{}': expected dim={} but got {}",
                self.name,
                self.dim,
                vec.len()
            )));
        }
        self.hnsw.write().unwrap().insert(rowid, vec);
        Ok(())
    }

    /// Lazily delete the vector for the given rowid.
    pub fn delete_vec(&self, rowid: u64) {
        self.hnsw.write().unwrap().lazy_delete(rowid);
    }

    /// Search for the `top_k` nearest neighbours of `query`.
    ///
    /// Returns `(rowid, similarity_score)` pairs sorted descending by score.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(u64, f32)> {
        self.hnsw.read().unwrap().search(query, top_k)
    }

    /// Like `search` but uses `ef_override` as the HNSW ef_search candidate set size.
    /// Set via `SET kkdb.vec_ef_search = N` to trade off speed vs recall.
    pub fn search_with_ef(
        &self,
        query: &[f32],
        top_k: usize,
        ef_override: usize,
    ) -> Vec<(u64, f32)> {
        let mut graph = self.hnsw.write().unwrap();
        let old_ef = graph.ef_search;
        graph.ef_search = ef_override.max(top_k);
        let results = graph.search(query, top_k);
        graph.ef_search = old_ef;
        results
    }

    /// Number of logically live entries (total - deleted).
    pub fn live_count(&self) -> usize {
        self.hnsw.read().unwrap().len()
    }

    /// Number of lazily-deleted entries that have not yet been purged.
    pub fn deleted_count(&self) -> usize {
        self.hnsw.read().unwrap().deleted_count()
    }
}

// ─── Registry ────────────────────────────────────────────────────────────────

/// In-memory registry of all active vector indexes (held by `Schema`).
#[derive(Debug, Default, Clone)]
pub struct VectorIndexRegistry {
    /// index_name (lowercase) → VectorIndex
    indexes: HashMap<String, VectorIndex>,
    /// table_name (lowercase) → list of index names
    by_table: HashMap<String, Vec<String>>,
    /// Next index_id to allocate (monotonically increasing).
    next_id: u32,
}

impl VectorIndexRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next unique numeric index ID.
    pub fn alloc_index_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Register a new vector index.
    pub fn register(&mut self, vi: VectorIndex) {
        let name_lower = vi.name.to_lowercase();
        let table_lower = vi.table.to_lowercase();
        self.by_table
            .entry(table_lower)
            .or_default()
            .push(name_lower.clone());
        self.indexes.insert(name_lower, vi);
    }

    /// Remove a vector index by name.
    pub fn drop(&mut self, name: &str) -> Option<VectorIndex> {
        let lower = name.to_lowercase();
        if let Some(vi) = self.indexes.remove(&lower) {
            let tbl = vi.table.to_lowercase();
            if let Some(list) = self.by_table.get_mut(&tbl) {
                list.retain(|n| *n != lower);
            }
            Some(vi)
        } else {
            None
        }
    }

    /// Look up a vector index by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&VectorIndex> {
        self.indexes.get(&name.to_lowercase())
    }

    /// Get all vector indexes defined on `table_name`.
    pub fn for_table(&self, table_name: &str) -> Vec<&VectorIndex> {
        let lower = table_name.to_lowercase();
        self.by_table
            .get(&lower)
            .map(|names| names.iter().filter_map(|n| self.indexes.get(n)).collect())
            .unwrap_or_default()
    }

    /// Iterator over all registered vector indexes.
    pub fn iter(&self) -> impl Iterator<Item = &VectorIndex> {
        self.indexes.values()
    }

    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }
}

// ─── VEC() blob helpers ──────────────────────────────────────────────────────

/// Parse a JSON array string like `"[0.1, 0.2, 0.3]"` into a `Vec<f32>`.
///
/// Supports both `[a, b, c]` and space/comma delimited forms.
pub fn parse_vec_json(s: &str) -> Option<Vec<f32>> {
    let trimmed = s.trim();
    // Strip surrounding brackets if present.
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    let nums: Option<Vec<f32>> = inner
        .split(',')
        .map(|part| part.trim().parse::<f32>().ok())
        .collect();
    let v = nums?;
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vec_json() {
        let v = parse_vec_json("[1.0, 2.0, 3.0]").unwrap();
        assert_eq!(v, vec![1.0f32, 2.0, 3.0]);
    }

    #[test]
    fn test_registry_register_and_lookup() {
        let mut reg = VectorIndexRegistry::new();
        let id = reg.alloc_index_id();
        let vi = VectorIndex::new(
            "idx_emb".to_string(),
            "articles".to_string(),
            "embedding".to_string(),
            2,
            4,
            DistanceMetric::Cosine,
            id,
        );
        reg.register(vi);
        assert!(reg.get("idx_emb").is_some());
        assert!(reg.get("IDX_EMB").is_some()); // case-insensitive
        assert_eq!(reg.for_table("ARTICLES").len(), 1);
    }

    #[test]
    fn test_insert_and_search_via_index() {
        let mut reg = VectorIndexRegistry::new();
        let id = reg.alloc_index_id();
        let vi = VectorIndex::new(
            "idx".to_string(),
            "t".to_string(),
            "v".to_string(),
            0,
            3,
            DistanceMetric::Cosine,
            id,
        );
        reg.register(vi);
        let vi = reg.get("idx").unwrap();
        vi.insert_vec(1, vec![1.0, 0.0, 0.0]).unwrap();
        vi.insert_vec(2, vec![0.0, 1.0, 0.0]).unwrap();
        vi.insert_vec(3, vec![0.0, 0.0, 1.0]).unwrap();

        let results = vi.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results[0].0, 1);
        assert!(results[0].1 > 0.99);
    }
}
