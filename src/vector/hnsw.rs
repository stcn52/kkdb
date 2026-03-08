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
use std::cmp::Ordering;

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
    fn eq(&self, other: &Self) -> bool { self.0.node_id == other.0.node_id }
}
impl Eq for MinCandidate {}
impl PartialOrd for MinCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for MinCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: smallest distance at top
        self.0.distance
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
        let Some(ep) = ep else { return vec![]; };

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
                let score = self.distance.similarity(query, self.vectors.get(&c.node_id).map_or(&[], |v| v));
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
        candidates.push(MinCandidate(Candidate { distance: entry_dist, node_id: entry }));
        // result: max-heap of ef best (worst at top for cheap eviction)
        let mut result: BinaryHeap<Candidate> = BinaryHeap::new();
        result.push(Candidate { distance: entry_dist, node_id: entry });

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
                        candidates.push(MinCandidate(Candidate { distance: d, node_id: nb }));
                        result.push(Candidate { distance: d, node_id: nb });
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
        out.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(Ordering::Equal));
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
}
