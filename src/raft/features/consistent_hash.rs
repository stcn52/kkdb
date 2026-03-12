// ── Consistent Hash Ring ─────────────────────────────────────────────────────
//
// Provides a consistent hashing implementation for shard routing
// in distributed kkdb deployments.
//
// Features:
//   - Configurable virtual nodes (vnodes) per physical node
//   - Deterministic key → node mapping
//   - Minimal disruption on node add/remove
//   - FNV-1a hashing for fast lookups
//
// ## Usage
//
// ```rust
// let mut ring = HashRing::new(150); // 150 vnodes per node
// ring.add_node("node-1");
// ring.add_node("node-2");
// ring.add_node("node-3");
// let target = ring.get_node("user:12345"); // → "node-2"
// ```

use std::collections::BTreeMap;

/// A consistent hash ring for routing keys to nodes.
#[derive(Debug, Clone)]
pub struct HashRing {
    ring: BTreeMap<u64, String>,
    nodes: Vec<String>,
    vnodes_per_node: u32,
}

impl HashRing {
    /// Create a new empty hash ring.
    ///
    /// `vnodes_per_node` controls how many virtual nodes each physical node
    /// maps to. Higher values (100-200) give better distribution but use
    /// more memory.
    pub fn new(vnodes_per_node: u32) -> Self {
        Self {
            ring: BTreeMap::new(),
            nodes: Vec::new(),
            vnodes_per_node: vnodes_per_node.max(1),
        }
    }

    /// Add a node to the ring.
    pub fn add_node(&mut self, node_id: &str) {
        if self.nodes.iter().any(|n| n == node_id) {
            return; // already present
        }
        self.nodes.push(node_id.to_string());
        for i in 0..self.vnodes_per_node {
            let vnode_key = format!("{}#{}", node_id, i);
            let hash = Self::hash(vnode_key.as_bytes());
            self.ring.insert(hash, node_id.to_string());
        }
    }

    /// Remove a node from the ring.
    pub fn remove_node(&mut self, node_id: &str) {
        self.nodes.retain(|n| n != node_id);
        for i in 0..self.vnodes_per_node {
            let vnode_key = format!("{}#{}", node_id, i);
            let hash = Self::hash(vnode_key.as_bytes());
            self.ring.remove(&hash);
        }
    }

    /// Look up which node a key should be routed to.
    ///
    /// Returns `None` if the ring is empty.
    pub fn get_node(&self, key: &str) -> Option<&str> {
        if self.ring.is_empty() {
            return None;
        }
        let hash = Self::hash(key.as_bytes());
        // Find the first vnode with hash >= key hash (clockwise walk)
        let node = self
            .ring
            .range(hash..)
            .next()
            .or_else(|| self.ring.iter().next()) // wrap around
            .map(|(_, v)| v.as_str());
        node
    }

    /// Get the N nodes responsible for a key (for replication).
    ///
    /// Returns up to `n` distinct physical nodes by walking clockwise
    /// from the key's position on the ring.
    pub fn get_nodes(&self, key: &str, n: usize) -> Vec<&str> {
        if self.ring.is_empty() || n == 0 {
            return Vec::new();
        }
        let hash = Self::hash(key.as_bytes());
        let mut result: Vec<&str> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Walk clockwise from hash
        for (_, node) in self.ring.range(hash..) {
            if seen.insert(node.as_str()) {
                result.push(node.as_str());
                if result.len() >= n {
                    return result;
                }
            }
        }
        // Wrap around from beginning
        for (_, node) in self.ring.iter() {
            if seen.insert(node.as_str()) {
                result.push(node.as_str());
                if result.len() >= n {
                    return result;
                }
            }
        }
        result
    }

    /// Number of physical nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of virtual nodes (total points on ring).
    pub fn vnode_count(&self) -> usize {
        self.ring.len()
    }

    /// List all physical nodes.
    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }

    /// Compute the distribution of keys across nodes.
    ///
    /// Given a set of keys, returns a map of node_id → count.
    pub fn distribution(&self, keys: &[&str]) -> std::collections::HashMap<String, usize> {
        let mut dist = std::collections::HashMap::new();
        for key in keys {
            if let Some(node) = self.get_node(key) {
                *dist.entry(node.to_string()).or_insert(0) += 1;
            }
        }
        dist
    }

    /// FNV-1a 64-bit hash.
    fn hash(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in data {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
}

/// R11: Shard router — maps table+key to a target shard/node.
///
/// Uses consistent hashing internally to determine which shard
/// should handle a given (table, key) combination.
#[derive(Debug, Clone)]
pub struct ShardRouter {
    ring: HashRing,
    /// Number of shards (for modular fallback).
    num_shards: usize,
}

impl ShardRouter {
    /// Create a new shard router with the given shard count.
    pub fn new(shard_names: &[&str], vnodes: u32) -> Self {
        let mut ring = HashRing::new(vnodes);
        for name in shard_names {
            ring.add_node(name);
        }
        Self {
            ring,
            num_shards: shard_names.len(),
        }
    }

    /// Route a (table, key) pair to a shard.
    pub fn route(&self, table: &str, key: &str) -> Option<&str> {
        let combined = format!("{}:{}", table.to_ascii_lowercase(), key);
        self.ring.get_node(&combined)
    }

    /// Route for replication: get the primary + N-1 replicas.
    pub fn route_replicated(&self, table: &str, key: &str, replicas: usize) -> Vec<&str> {
        let combined = format!("{}:{}", table.to_ascii_lowercase(), key);
        self.ring.get_nodes(&combined, replicas)
    }

    /// Add a new shard.
    pub fn add_shard(&mut self, shard_name: &str) {
        self.ring.add_node(shard_name);
        self.num_shards += 1;
    }

    /// Remove a shard.
    pub fn remove_shard(&mut self, shard_name: &str) {
        self.ring.remove_node(shard_name);
        self.num_shards = self.num_shards.saturating_sub(1);
    }

    /// Number of shards.
    pub fn shard_count(&self) -> usize {
        self.ring.node_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_ring_empty() {
        let ring = HashRing::new(10);
        assert!(ring.get_node("key").is_none());
        assert_eq!(ring.node_count(), 0);
    }

    #[test]
    fn hash_ring_single_node() {
        let mut ring = HashRing::new(10);
        ring.add_node("node-1");
        assert_eq!(ring.get_node("any_key"), Some("node-1"));
        assert_eq!(ring.get_node("other_key"), Some("node-1"));
        assert_eq!(ring.node_count(), 1);
        assert_eq!(ring.vnode_count(), 10);
    }

    #[test]
    fn hash_ring_consistent_routing() {
        let mut ring = HashRing::new(100);
        ring.add_node("a");
        ring.add_node("b");
        ring.add_node("c");
        // Same key always maps to the same node
        let n1 = ring.get_node("user:123").unwrap();
        let n2 = ring.get_node("user:123").unwrap();
        assert_eq!(n1, n2);
    }

    #[test]
    fn hash_ring_add_node_minimal_disruption() {
        let mut ring = HashRing::new(100);
        ring.add_node("a");
        ring.add_node("b");
        // Record mapping for 100 keys
        let keys: Vec<String> = (0..100).map(|i| format!("key_{i}")).collect();
        let before: Vec<String> = keys
            .iter()
            .map(|k| ring.get_node(k).unwrap().to_string())
            .collect();
        // Add a third node
        ring.add_node("c");
        let after: Vec<String> = keys
            .iter()
            .map(|k| ring.get_node(k).unwrap().to_string())
            .collect();
        // Most keys should stay on the same node
        let same = before
            .iter()
            .zip(after.iter())
            .filter(|(a, b)| a == b)
            .count();
        // At least some keys should stay on the same node (>0)
        assert!(same > 0, "Expected some keys unchanged, got {same}/100");
    }

    #[test]
    fn hash_ring_remove_node() {
        let mut ring = HashRing::new(10);
        ring.add_node("a");
        ring.add_node("b");
        assert_eq!(ring.node_count(), 2);
        ring.remove_node("a");
        assert_eq!(ring.node_count(), 1);
        // All keys should now map to "b"
        assert_eq!(ring.get_node("test"), Some("b"));
    }

    #[test]
    fn hash_ring_get_nodes_replication() {
        let mut ring = HashRing::new(50);
        ring.add_node("n1");
        ring.add_node("n2");
        ring.add_node("n3");
        let nodes = ring.get_nodes("key1", 2);
        assert_eq!(nodes.len(), 2);
        assert_ne!(nodes[0], nodes[1]);
    }

    #[test]
    fn hash_ring_get_nodes_exceeds_count() {
        let mut ring = HashRing::new(50);
        ring.add_node("n1");
        ring.add_node("n2");
        let nodes = ring.get_nodes("key1", 5);
        assert_eq!(nodes.len(), 2); // only 2 physical nodes
    }

    #[test]
    fn hash_ring_distribution() {
        let mut ring = HashRing::new(150);
        ring.add_node("s1");
        ring.add_node("s2");
        ring.add_node("s3");
        let keys: Vec<String> = (0..300).map(|i| format!("item_{i}")).collect();
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        let dist = ring.distribution(&key_refs);
        // Each node should get approximately 100 keys
        for count in dist.values() {
            assert!(*count > 5, "Distribution too skewed: {count}/300");
        }
    }

    #[test]
    fn hash_ring_duplicate_add() {
        let mut ring = HashRing::new(10);
        ring.add_node("a");
        ring.add_node("a"); // duplicate
        assert_eq!(ring.node_count(), 1);
        assert_eq!(ring.vnode_count(), 10); // not 20
    }

    #[test]
    fn shard_router_basic() {
        let router = ShardRouter::new(&["shard-0", "shard-1", "shard-2"], 100);
        assert_eq!(router.shard_count(), 3);
        let target = router.route("users", "42");
        assert!(target.is_some());
    }

    #[test]
    fn shard_router_replicated() {
        let router = ShardRouter::new(&["s0", "s1", "s2"], 100);
        let nodes = router.route_replicated("orders", "key-99", 2);
        assert_eq!(nodes.len(), 2);
        assert_ne!(nodes[0], nodes[1]);
    }

    #[test]
    fn shard_router_add_remove() {
        let mut router = ShardRouter::new(&["a", "b"], 50);
        assert_eq!(router.shard_count(), 2);
        router.add_shard("c");
        assert_eq!(router.shard_count(), 3);
        router.remove_shard("a");
        assert_eq!(router.shard_count(), 2);
    }
}
