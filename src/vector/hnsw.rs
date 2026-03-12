use std::cmp::Ordering;
/// HNSW (Hierarchical Navigable Small World) graph implementation.
///
/// Key properties:
/// - O(log N) approximate nearest-neighbor search.
/// - In-memory graph; vector data persisted separately in B-Tree (Phase 2+).
/// - Lazy deletion: deleted nodes are skipped during search; graph is rebuilt
///   when the deletion ratio exceeds `REBUILD_THRESHOLD`.
///
/// References:
///   Malkov & Yashunin (2018), https://arxiv.org/abs/1603.09320
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::vector::distance::DistanceMetric;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Default M (max neighbours per node above layer 0).
pub const DEFAULT_M: usize = 16;
/// Default M for layer 0 (= 2 × M).
pub const DEFAULT_M_MAX0: usize = 32;
/// Default ef_construction (candidate set size during insert).
pub const DEFAULT_EF_CONSTRUCTION: usize = 200;
/// Default ef_search (candidate set size during query).
pub const DEFAULT_EF_SEARCH: usize = 50;
/// Trigger a full rebuild when deleted fraction exceeds this.
const REBUILD_THRESHOLD: f32 = 0.2;

// ─── Heap entry ──────────────────────────────────────────────────────────────

/// Wrapper so we can use `BinaryHeap` as a min-heap or max-heap by wrapping distance.
#[derive(Clone)]
struct Candidate {
    /// Lower distance = better match.
    distance: f32,
    node_id: u64,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.node_id == other.node_id
    }
}
impl Eq for Candidate {}

/// Max-heap by distance (used for candidates / result sets, where we want to evict the *worst*).
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse: largest distance at top of heap (so we can pop the worst)
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(Ordering::Equal)
    }
}

/// Min-heap wrapper (lower distance = higher priority, for greedy descent).
struct MinCandidate(Candidate);

impl PartialEq for MinCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.0.node_id == other.0.node_id
    }
}
impl Eq for MinCandidate {}
impl PartialOrd for MinCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MinCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: smallest distance at top
        self.0
            .distance
            .partial_cmp(&other.0.distance)
            .unwrap_or(Ordering::Equal)
            .reverse()
    }
}

// ─── HNSW Graph ──────────────────────────────────────────────────────────────

/// The HNSW graph: all nodes live in memory.
pub struct HnswGraph {
    /// node_id (rowid) → adjacency list per layer.
    /// nodes[id][layer] = list of neighbour node_ids at that layer.
    nodes: HashMap<u64, Vec<Vec<u64>>>,
    /// node_id → raw f32 vector (cached in memory for distance computation).
    pub vectors: HashMap<u64, Vec<f32>>,
    /// Lazily-deleted node ids (filtered during search, triggers rebuild when dense).
    deleted: HashSet<u64>,
    /// Entry point for the search traversal (node_id in the highest layer).
    entry_point: Option<u64>,
    /// Current maximum layer index (0-based).
    max_level: usize,

    // ── Hyperparameters ──
    pub m: usize,
    pub m_max0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub distance: DistanceMetric,
}

impl HnswGraph {
    /// Create a new empty graph.
    pub fn new(m: usize, ef_construction: usize, distance: DistanceMetric) -> Self {
        Self {
            nodes: HashMap::new(),
            vectors: HashMap::new(),
            deleted: HashSet::new(),
            entry_point: None,
            max_level: 0,
            m,
            m_max0: m * 2,
            ef_construction,
            ef_search: DEFAULT_EF_SEARCH,
            distance,
        }
    }

    /// Number of non-deleted nodes.
    pub fn len(&self) -> usize {
        self.nodes.len().saturating_sub(self.deleted.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of lazily-deleted nodes pending cleanup.
    pub fn deleted_count(&self) -> usize {
        self.deleted.len()
    }

    /// Total nodes in the graph including deleted ones.
    pub fn total_count(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the deletion fraction is large enough to warrant a rebuild.
    pub fn should_rebuild(&self) -> bool {
        let total = self.nodes.len();
        total > 0 && (self.deleted.len() as f32 / total as f32) > REBUILD_THRESHOLD
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Insert a new vector with the given `rowid`.
    ///
    /// If `rowid` already exists, it is first lazily deleted and re-inserted.
    pub fn insert(&mut self, rowid: u64, vec: Vec<f32>) {
        // If updating an existing node, lazy-delete the old position first.
        if self.nodes.contains_key(&rowid) {
            self.deleted.insert(rowid);
        }

        // Determine the random level for this node.
        let level = self.random_level();

        // Store the vector.
        self.vectors.insert(rowid, vec.clone());

        // Initialise adjacency lists (one per layer).
        let adjacency: Vec<Vec<u64>> = (0..=level).map(|_| Vec::new()).collect();
        self.nodes.insert(rowid, adjacency);

        if self.entry_point.is_none() {
            // First node — it becomes the entry point at layer 0.
            self.entry_point = Some(rowid);
            self.max_level = level;
            return;
        }

        let ep = self.entry_point.unwrap();

        // ── Phase 1: Greedy descent from max_level down to level+1 ──
        let mut ep_cur = ep;
        for lc in (level + 1..=self.max_level).rev() {
            let candidates = self.search_layer(&vec, ep_cur, 1, lc);
            if let Some(best) = candidates.into_iter().next() {
                ep_cur = best.node_id;
            }
        }

        // ── Phase 2: Bidirectional connect at each layer [level..0] ──
        let connect_layers = level.min(self.max_level);
        for lc in (0..=connect_layers).rev() {
            let ef = self.ef_construction;
            let candidates = self.search_layer(&vec, ep_cur, ef, lc);
            if let Some(best) = candidates.first() {
                ep_cur = best.node_id;
            }

            // Select M neighbours (or M_max0 for layer 0).
            let m_layer = if lc == 0 { self.m_max0 } else { self.m };
            let selected = select_neighbours_greedy(&candidates, m_layer);

            // Connect new node → selected neighbours.
            if let Some(adj) = self.nodes.get_mut(&rowid) {
                while adj.len() <= lc {
                    adj.push(Vec::new());
                }
                adj[lc] = selected.iter().map(|c| c.node_id).collect();
            }

            // Connect selected neighbours → new node (bidirectional), then prune.
            for nb in &selected {
                // Step 1: add edge (mutable borrow of nodes).
                let needs_prune = {
                    if let Some(nb_adj) = self.nodes.get_mut(&nb.node_id) {
                        while nb_adj.len() <= lc {
                            nb_adj.push(Vec::new());
                        }
                        nb_adj[lc].push(rowid);
                        nb_adj[lc].len() > m_layer
                    } else {
                        false
                    }
                }; // mutable borrow released here

                // Step 2: if over-full, compute pruned list using immutable borrows.
                if needs_prune {
                    let nb_id = nb.node_id;
                    let nb_vec = self.vectors.get(&nb_id).cloned().unwrap_or_default();
                    let current_neighbours: Vec<u64> = self
                        .nodes
                        .get(&nb_id)
                        .and_then(|adj| adj.get(lc))
                        .cloned()
                        .unwrap_or_default();
                    let over: Vec<Candidate> = current_neighbours
                        .iter()
                        .filter_map(|&cid| {
                            self.vectors.get(&cid).map(|cv| Candidate {
                                distance: self.distance.distance(&nb_vec, cv),
                                node_id: cid,
                            })
                        })
                        .collect();
                    let pruned: Vec<u64> = select_neighbours_greedy(&over, m_layer)
                        .iter()
                        .map(|c| c.node_id)
                        .collect();

                    // Step 3: write back pruned list (mutable borrow again).
                    if let Some(nb_adj) = self.nodes.get_mut(&nb_id) {
                        if let Some(layer_adj) = nb_adj.get_mut(lc) {
                            *layer_adj = pruned;
                        }
                    }
                }
            }
        }

        // Update entry point if this node reaches a higher level.
        if level > self.max_level {
            self.max_level = level;
            self.entry_point = Some(rowid);
        }
    }

    /// Mark `rowid` as lazily deleted.
    pub fn lazy_delete(&mut self, rowid: u64) {
        self.deleted.insert(rowid);
    }

    /// Approximate KNN search: returns up to `top_k` `(rowid, score)` pairs,
    /// sorted descending by *similarity* (higher = better).
    ///
    /// Deleted nodes are filtered out of results.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(u64, f32)> {
        let Some(ep) = self.entry_point else {
            return vec![];
        };
        if self.deleted.contains(&ep) && self.nodes.len() == self.deleted.len() {
            return vec![];
        }

        // Find a valid entry point (skip if deleted).
        let ep = self.find_valid_entry(ep, query);
        let Some(ep) = ep else {
            return vec![];
        };

        // Greedy descent layers max_level..1
        let mut ep_cur = ep;
        for lc in (1..=self.max_level).rev() {
            let cands = self.search_layer(query, ep_cur, 1, lc);
            if let Some(best) = cands.first() {
                if !self.deleted.contains(&best.node_id) {
                    ep_cur = best.node_id;
                }
            }
        }

        // Detailed search at layer 0.
        let ef = self.ef_search.max(top_k);
        let candidates = self.search_layer(query, ep_cur, ef, 0);

        // Convert to (rowid, similarity_score), filter deleted.
        let mut results: Vec<(u64, f32)> = candidates
            .into_iter()
            .filter(|c| !self.deleted.contains(&c.node_id))
            .take(top_k)
            .map(|c| {
                let score = self
                    .distance
                    .similarity(query, self.vectors.get(&c.node_id).map_or(&[], |v| v));
                (c.node_id, score)
            })
            .collect();

        // Sort descending by score.
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        results
    }

    /// Rebuild the entire graph from a fresh iterator of `(rowid, vec)` pairs.
    ///
    /// Used after lazy deletion exceeds threshold, or at startup from B-Tree scan.
    pub fn rebuild_from_iter(&mut self, iter: impl Iterator<Item = (u64, Vec<f32>)>) {
        self.nodes.clear();
        self.vectors.clear();
        self.deleted.clear();
        self.entry_point = None;
        self.max_level = 0;
        for (rowid, vec) in iter {
            self.insert(rowid, vec);
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Random level using an exponential distribution: P(l) ≈ exp(-l / level_mult).
    fn random_level(&self) -> usize {
        use std::time::SystemTime;
        // Simple xorshift64 seeded from wall clock nanos (no rand crate needed).
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(42)
            ^ (self.nodes.len() as u64).wrapping_mul(0x9e3779b97f4a7c15);
        let mut x = seed | 1;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let f = (x >> 11) as f64 / (1u64 << 53) as f64; // uniform [0,1)
        let level_mult = 1.0 / (self.m as f64).ln();
        let level = (-f.ln() * level_mult).floor() as usize;
        level.min(16) // cap at 16 layers
    }

    /// Greedy search within a single layer, returning up to `ef` best candidates
    /// as a max-heap ordered by distance (index 0 = closest after sorting).
    fn search_layer(&self, query: &[f32], entry: u64, ef: usize, layer: usize) -> Vec<Candidate> {
        // visited set
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(entry);

        let entry_dist = self.dist(query, entry);
        // candidates: min-heap (closest at top for greedy expansion)
        let mut candidates: BinaryHeap<MinCandidate> = BinaryHeap::new();
        candidates.push(MinCandidate(Candidate {
            distance: entry_dist,
            node_id: entry,
        }));
        // result: max-heap of ef best (worst at top for cheap eviction)
        let mut result: BinaryHeap<Candidate> = BinaryHeap::new();
        result.push(Candidate {
            distance: entry_dist,
            node_id: entry,
        });

        while let Some(MinCandidate(cur)) = candidates.pop() {
            // If the worst result is closer than the best candidate, we're done.
            let worst_result_dist = result.peek().map(|c| c.distance).unwrap_or(f32::MAX);
            if cur.distance > worst_result_dist && result.len() >= ef {
                break;
            }

            // Expand neighbours at this layer.
            if let Some(adj) = self.nodes.get(&cur.node_id) {
                let neighbours = adj.get(layer).map(|v| v.as_slice()).unwrap_or(&[]);
                for &nb in neighbours {
                    if visited.contains(&nb) {
                        continue;
                    }
                    visited.insert(nb);

                    let d = self.dist(query, nb);
                    let worst = result.peek().map(|c| c.distance).unwrap_or(f32::MAX);
                    if d < worst || result.len() < ef {
                        candidates.push(MinCandidate(Candidate {
                            distance: d,
                            node_id: nb,
                        }));
                        result.push(Candidate {
                            distance: d,
                            node_id: nb,
                        });
                        // Keep result bounded to ef.
                        if result.len() > ef {
                            result.pop();
                        }
                    }
                }
            }
        }

        // Return as a sorted Vec (closest first).
        let mut out: Vec<Candidate> = result.into_vec();
        out.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });
        out
    }

    /// Compute distance from `query` to the vector stored at `node_id`.
    fn dist(&self, query: &[f32], node_id: u64) -> f32 {
        self.vectors
            .get(&node_id)
            .map(|v| self.distance.distance(query, v))
            .unwrap_or(f32::MAX)
    }

    /// Walk from `ep` looking for a non-deleted node (needed when ep itself is deleted).
    fn find_valid_entry(&self, ep: u64, _query: &[f32]) -> Option<u64> {
        if !self.deleted.contains(&ep) {
            return Some(ep);
        }
        // Fall back: scan adjacency of ep at layer 0.
        if let Some(adj) = self.nodes.get(&ep) {
            if let Some(layer0) = adj.first() {
                for &nb in layer0 {
                    if !self.deleted.contains(&nb) {
                        return Some(nb);
                    }
                }
            }
        }
        // Last resort: linear scan (only when graph is nearly fully deleted).
        self.nodes
            .keys()
            .find(|&&id| !self.deleted.contains(&id))
            .copied()
    }

    // ── Optimization: Search with custom ef ────────────────────────────────

    /// Search with a custom `ef` parameter for recall/speed trade-off.
    ///
    /// Higher `ef` → better recall but slower search.
    /// Lower  `ef` → faster search but may miss nearest neighbours.
    pub fn search_with_ef(&self, query: &[f32], top_k: usize, ef: usize) -> Vec<(u64, f32)> {
        let Some(ep) = self.entry_point else {
            return vec![];
        };
        if self.deleted.contains(&ep) && self.nodes.len() == self.deleted.len() {
            return vec![];
        }

        let ep = self.find_valid_entry(ep, query);
        let Some(ep) = ep else {
            return vec![];
        };

        let mut ep_cur = ep;
        for lc in (1..=self.max_level).rev() {
            let cands = self.search_layer(query, ep_cur, 1, lc);
            if let Some(best) = cands.first() {
                if !self.deleted.contains(&best.node_id) {
                    ep_cur = best.node_id;
                }
            }
        }

        let candidates = self.search_layer(query, ep_cur, ef.max(top_k), 0);

        let mut results: Vec<(u64, f32)> = candidates
            .into_iter()
            .filter(|c| !self.deleted.contains(&c.node_id))
            .take(top_k)
            .map(|c| {
                let score = self
                    .distance
                    .similarity(query, self.vectors.get(&c.node_id).map_or(&[], |v| v));
                (c.node_id, score)
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        results
    }

    // ── Optimization: Batch insert ────────────────────────────────────────

    /// Insert multiple vectors in batch.
    ///
    /// More efficient than individual inserts when loading large datasets,
    /// because the graph structure benefits from having more nodes available
    /// during connection selection.
    pub fn batch_insert(&mut self, items: Vec<(u64, Vec<f32>)>) {
        for (rowid, vec) in items {
            self.insert(rowid, vec);
        }
    }

    // ── Persistence: Serialize/Deserialize ────────────────────────────────

    /// Serialize the graph to a portable binary format for persistence.
    ///
    /// Format:
    /// ```text
    /// [4 bytes] node count (u32 LE)
    /// [4 bytes] dimension (u32 LE)
    /// [1 byte]  distance metric (0=Cosine, 1=L2, 2=DotProduct)
    /// [4 bytes] M (u32 LE)
    /// [4 bytes] ef_construction (u32 LE)
    /// [4 bytes] ef_search (u32 LE)
    /// For each node:
    ///   [8 bytes] rowid (u64 LE)
    ///   [4 bytes] num_layers (u32 LE)
    ///   [dim*4 bytes] vector data (f32 LE × dim)
    ///   For each layer:
    ///     [4 bytes] neighbour count (u32 LE)
    ///     [count*8 bytes] neighbour rowids (u64 LE each)
    /// ```
    pub fn serialize(&self) -> Vec<u8> {
        let dim = self.vectors.values().next().map(|v| v.len() as u32).unwrap_or(0);
        // Count only non-deleted nodes
        let node_count = self.nodes.keys().filter(|k| !self.deleted.contains(k)).count() as u32;

        let mut buf = Vec::new();
        buf.extend_from_slice(&node_count.to_le_bytes());
        buf.extend_from_slice(&dim.to_le_bytes());
        buf.push(match self.distance {
            DistanceMetric::Cosine => 0,
            DistanceMetric::L2 => 1,
        });
        buf.extend_from_slice(&(self.m as u32).to_le_bytes());
        buf.extend_from_slice(&(self.ef_construction as u32).to_le_bytes());
        buf.extend_from_slice(&(self.ef_search as u32).to_le_bytes());

        // Serialize each non-deleted node
        for (&rowid, adj) in &self.nodes {
            if self.deleted.contains(&rowid) {
                continue;
            }
            buf.extend_from_slice(&rowid.to_le_bytes());
            buf.extend_from_slice(&(adj.len() as u32).to_le_bytes());

            // Vector data
            if let Some(vec) = self.vectors.get(&rowid) {
                for &v in vec {
                    buf.extend_from_slice(&v.to_le_bytes());
                }
            }

            // Adjacency lists per layer
            for layer in adj {
                let count = layer.len() as u32;
                buf.extend_from_slice(&count.to_le_bytes());
                for &nb in layer {
                    buf.extend_from_slice(&nb.to_le_bytes());
                }
            }
        }

        buf
    }

    /// Deserialize a graph from the binary format produced by `serialize()`.
    ///
    /// Returns `None` if the data is too short or corrupt.
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 21 {
            return None;
        }

        let mut pos = 0;

        let node_count = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        let dim = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        let distance = match data[pos] {
            0 => DistanceMetric::Cosine,
            1 => DistanceMetric::L2,
            _ => return None,
        };
        pos += 1;
        let m = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        let ef_construction = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        let ef_search = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;

        let mut graph = Self {
            nodes: HashMap::new(),
            vectors: HashMap::new(),
            deleted: HashSet::new(),
            entry_point: None,
            max_level: 0,
            m,
            m_max0: m * 2,
            ef_construction,
            ef_search,
            distance,
        };

        let mut first_id = None;

        for _ in 0..node_count {
            if pos + 12 > data.len() {
                return None;
            }

            let rowid = u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
            pos += 8;
            let num_layers = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
            pos += 4;

            // Read vector
            let vec_bytes = dim * 4;
            if pos + vec_bytes > data.len() {
                return None;
            }
            let mut vec = Vec::with_capacity(dim);
            for _ in 0..dim {
                let v = f32::from_le_bytes(data[pos..pos + 4].try_into().ok()?);
                pos += 4;
                vec.push(v);
            }
            graph.vectors.insert(rowid, vec);

            // Read adjacency
            let mut adj = Vec::with_capacity(num_layers);
            for _ in 0..num_layers {
                if pos + 4 > data.len() {
                    return None;
                }
                let count = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
                pos += 4;
                let mut neighbours = Vec::with_capacity(count);
                for _ in 0..count {
                    if pos + 8 > data.len() {
                        return None;
                    }
                    let nb = u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
                    pos += 8;
                    neighbours.push(nb);
                }
                adj.push(neighbours);
            }

            if num_layers > graph.max_level + 1 {
                graph.max_level = num_layers - 1;
            }
            if first_id.is_none() {
                first_id = Some(rowid);
            }

            graph.nodes.insert(rowid, adj);
        }

        graph.entry_point = first_id;
        Some(graph)
    }

    /// Get graph statistics for monitoring.
    pub fn graph_stats(&self) -> HnswStats {
        let total_edges: usize = self
            .nodes
            .values()
            .map(|adj| adj.iter().map(|layer| layer.len()).sum::<usize>())
            .sum();
        let active_nodes = self.len();
        HnswStats {
            total_nodes: self.nodes.len(),
            active_nodes,
            deleted_nodes: self.deleted.len(),
            max_level: self.max_level,
            total_edges,
            avg_edges_per_node: if active_nodes > 0 {
                total_edges as f64 / active_nodes as f64
            } else {
                0.0
            },
            entry_point: self.entry_point,
            m: self.m,
            ef_construction: self.ef_construction,
            ef_search: self.ef_search,
        }
    }
}

/// HNSW graph statistics.
#[derive(Debug, Clone)]
pub struct HnswStats {
    pub total_nodes: usize,
    pub active_nodes: usize,
    pub deleted_nodes: usize,
    pub max_level: usize,
    pub total_edges: usize,
    pub avg_edges_per_node: f64,
    pub entry_point: Option<u64>,
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
}

// ─── Free helpers (no self borrow, avoids E0502 in insert()) ─────────────────

/// Pick the `m` closest candidates (simple greedy selection by distance).
fn select_neighbours_greedy(candidates: &[Candidate], m: usize) -> Vec<Candidate> {
    candidates.iter().take(m).cloned().collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::distance::DistanceMetric;

    fn build_graph() -> HnswGraph {
        HnswGraph::new(4, 20, DistanceMetric::Cosine)
    }

    #[test]
    fn test_insert_and_search_basic() {
        let mut g = build_graph();
        g.insert(1, vec![1.0, 0.0, 0.0]);
        g.insert(2, vec![0.0, 1.0, 0.0]);
        g.insert(3, vec![0.0, 0.0, 1.0]);
        g.insert(4, vec![0.9, 0.1, 0.0]);

        let results = g.search(&[1.0, 0.0, 0.0], 2);
        assert!(!results.is_empty());
        // rowid 1 should be the closest to [1,0,0]
        assert_eq!(results[0].0, 1);
        // score should be very close to 1.0
        assert!(results[0].1 > 0.99, "score: {}", results[0].1);
    }

    #[test]
    fn test_lazy_delete() {
        let mut g = build_graph();
        g.insert(1, vec![1.0, 0.0]);
        g.insert(2, vec![0.9, 0.1]);
        g.insert(3, vec![0.0, 1.0]);

        g.lazy_delete(1);
        let results = g.search(&[1.0, 0.0], 3);
        // rowid 1 must not appear in results
        assert!(results.iter().all(|(id, _)| *id != 1));
    }

    #[test]
    fn test_rebuild() {
        let mut g = build_graph();
        for i in 1..=10u64 {
            g.insert(i, vec![i as f32, 0.0, 0.0]);
        }
        for i in 1..=8u64 {
            g.lazy_delete(i);
        }
        // should_rebuild: 8/10 > 0.2
        assert!(g.should_rebuild());
        let remaining = vec![
            (9u64, vec![9.0f32, 0.0, 0.0]),
            (10u64, vec![10.0f32, 0.0, 0.0]),
        ];
        g.rebuild_from_iter(remaining.into_iter());
        let results = g.search(&[9.0, 0.0, 0.0], 1);
        assert_eq!(results[0].0, 9);
    }

    #[test]
    fn test_l2_metric() {
        let mut g = HnswGraph::new(4, 20, DistanceMetric::L2);
        g.insert(1, vec![0.0, 0.0]);
        g.insert(2, vec![1.0, 0.0]);
        g.insert(3, vec![10.0, 0.0]);
        let results = g.search(&[0.1, 0.0], 1);
        assert_eq!(results[0].0, 1);
    }

    // ── New coverage tests ──────────────────────────────────────────────

    #[test]
    fn test_search_empty_graph() {
        let g = build_graph();
        let results = g.search(&[1.0, 0.0, 0.0], 5);
        assert!(results.is_empty(), "empty graph should return no results");
    }

    #[test]
    fn test_search_single_node() {
        let mut g = build_graph();
        g.insert(42, vec![1.0, 0.0]);
        let results = g.search(&[0.5, 0.5], 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 42);
    }

    #[test]
    fn test_should_rebuild_below_threshold() {
        let mut g = build_graph();
        for i in 1..=10u64 {
            g.insert(i, vec![i as f32, 0.0]);
        }
        // Delete only 1/10 = 10% < 20% threshold
        g.lazy_delete(1);
        assert!(!g.should_rebuild());
    }

    #[test]
    fn test_should_rebuild_empty_graph() {
        let g = build_graph();
        assert!(!g.should_rebuild(), "empty graph should not need rebuild");
    }

    #[test]
    fn test_insert_many_vectors() {
        // Verify HNSW handles 200+ insertions without panic and returns results
        let mut g = HnswGraph::new(16, 200, DistanceMetric::L2);
        for i in 0..200u64 {
            let angle = (i as f32) * 0.031415926;
            g.insert(i, vec![angle.cos(), angle.sin()]);
        }
        assert_eq!(g.len(), 200);
        let results = g.search(&[1.0, 0.0], 5);
        assert!(!results.is_empty(), "search should return results from 200-node graph");
        // id 0 has vector [cos(0), sin(0)] = [1.0, 0.0], should be in top-5
        assert!(
            results.iter().any(|(id, _)| *id == 0),
            "id 0 (nearest to [1,0]) should be in results: {:?}",
            results
        );
    }

    #[test]
    fn test_len_and_is_empty() {
        let mut g = build_graph();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
        g.insert(1, vec![1.0, 0.0]);
        assert!(!g.is_empty());
        assert_eq!(g.len(), 1);
    }

    // ── New optimization tests ──────────────────────────────────────────

    #[test]
    fn test_search_with_ef() {
        let mut g = HnswGraph::new(8, 50, DistanceMetric::L2);
        for i in 0..50u64 {
            g.insert(i, vec![i as f32, 0.0]);
        }
        // Low ef — fast but might miss
        let results_low = g.search_with_ef(&[25.0, 0.0], 3, 5);
        assert!(!results_low.is_empty());

        // High ef — better recall
        let results_high = g.search_with_ef(&[25.0, 0.0], 3, 100);
        assert!(!results_high.is_empty());
        assert!(results_high.len() <= 3);
    }

    #[test]
    fn test_batch_insert() {
        let mut g = HnswGraph::new(8, 50, DistanceMetric::L2);
        let items: Vec<(u64, Vec<f32>)> = (0..20)
            .map(|i| (i as u64, vec![i as f32, (20 - i) as f32]))
            .collect();
        g.batch_insert(items);
        assert_eq!(g.len(), 20);
        let results = g.search(&[10.0, 10.0], 3);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let mut g = HnswGraph::new(4, 20, DistanceMetric::Cosine);
        g.insert(1, vec![1.0, 0.0, 0.0]);
        g.insert(2, vec![0.0, 1.0, 0.0]);
        g.insert(3, vec![0.0, 0.0, 1.0]);

        let data = g.serialize();
        let g2 = HnswGraph::deserialize(&data).expect("deserialize should succeed");

        assert_eq!(g2.len(), 3);
        assert!(g2.vectors.contains_key(&1));
        assert!(g2.vectors.contains_key(&2));
        assert!(g2.vectors.contains_key(&3));

        // Search should work on deserialized graph
        let results = g2.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_serialize_deserialize_l2() {
        let mut g = HnswGraph::new(8, 50, DistanceMetric::L2);
        for i in 0..10u64 {
            g.insert(i, vec![i as f32, (i * 2) as f32]);
        }
        let data = g.serialize();
        let g2 = HnswGraph::deserialize(&data).unwrap();
        assert_eq!(g2.len(), 10);
        let stats = g2.graph_stats();
        assert_eq!(stats.active_nodes, 10);
        assert_eq!(stats.m, 8);
    }

    #[test]
    fn test_serialize_with_deleted_nodes() {
        let mut g = build_graph();
        g.insert(1, vec![1.0, 0.0]);
        g.insert(2, vec![0.0, 1.0]);
        g.insert(3, vec![1.0, 1.0]);
        g.lazy_delete(2);

        let data = g.serialize();
        let g2 = HnswGraph::deserialize(&data).unwrap();
        // Only non-deleted nodes should be serialized (2 active)
        assert!(g2.len() >= 2);
        // Deleted node's vector should not be in deserialized graph
        assert!(!g2.vectors.contains_key(&2));
    }

    #[test]
    fn test_deserialize_invalid_data() {
        // Too short
        assert!(HnswGraph::deserialize(&[0u8; 10]).is_none());
        // Invalid distance metric
        let mut bad = vec![0u8; 21];
        bad[8] = 99; // invalid metric
        assert!(HnswGraph::deserialize(&bad).is_none());
    }

    #[test]
    fn test_graph_stats() {
        let mut g = HnswGraph::new(4, 20, DistanceMetric::Cosine);
        g.insert(1, vec![1.0, 0.0]);
        g.insert(2, vec![0.0, 1.0]);
        g.insert(3, vec![1.0, 1.0]);
        g.lazy_delete(3);

        let stats = g.graph_stats();
        assert_eq!(stats.total_nodes, 3);
        assert_eq!(stats.active_nodes, 2);
        assert_eq!(stats.deleted_nodes, 1);
        assert!(stats.entry_point.is_some());
        assert_eq!(stats.m, 4);
        assert_eq!(stats.ef_construction, 20);
    }

    #[test]
    fn test_serialize_empty_graph() {
        let g = build_graph();
        let data = g.serialize();
        let g2 = HnswGraph::deserialize(&data).unwrap();
        assert_eq!(g2.len(), 0);
        assert!(g2.is_empty());
    }

    #[test]
    fn test_search_with_ef_empty() {
        let g = build_graph();
        let results = g.search_with_ef(&[1.0], 5, 100);
        assert!(results.is_empty());
    }

    #[test]
    fn test_batch_insert_duplicate_update() {
        let mut g = build_graph();
        g.insert(1, vec![1.0, 0.0]);
        g.batch_insert(vec![(1, vec![0.0, 1.0])]); // update existing
        // vector should be updated
        let v = g.vectors.get(&1).unwrap();
        assert_eq!(v, &vec![0.0, 1.0]);
    }
}
