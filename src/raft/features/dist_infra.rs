//! 分布式基础设施 — 节点发现与注册 / 配置中心 / 服务网格 / 链路加密
//!
//! Round 23 feature module providing foundational distributed infrastructure:
//! - `NodeDiscovery` — node registration, heartbeat, service discovery
//! - `ConfigCenter` — distributed configuration management with versioning
//! - `ServiceMesh` — service routing with load awareness
//! - `LinkEncryption` — inter-node communication encryption

use std::collections::HashMap;

// ─── Node Discovery & Registration ───────────────────────────────────

/// Represents a discovered node in the cluster.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub node_id: String,
    pub address: String,
    pub port: u16,
    pub role: NodeRole,
    pub last_heartbeat_ms: u64,
    pub metadata: HashMap<String, String>,
}

/// Role of a node in the cluster.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeRole {
    Leader,
    Follower,
    Observer,
    Candidate,
}

/// Status of a discovered node.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeStatus {
    Healthy,
    Suspect,
    Dead,
    Joining,
}

/// Node discovery and registration service.
pub struct NodeDiscovery {
    nodes: HashMap<String, NodeInfo>,
    heartbeat_timeout_ms: u64,
    current_time_ms: u64,
}

impl NodeDiscovery {
    /// Create a new NodeDiscovery with the given heartbeat timeout.
    pub fn new(heartbeat_timeout_ms: u64) -> Self {
        Self {
            nodes: HashMap::new(),
            heartbeat_timeout_ms,
            current_time_ms: 0,
        }
    }

    /// Advance the logical clock.
    pub fn tick(&mut self, now_ms: u64) {
        self.current_time_ms = now_ms;
    }

    /// Register a node with the discovery service.
    pub fn register(&mut self, node_id: &str, address: &str, port: u16, role: NodeRole) {
        let info = NodeInfo {
            node_id: node_id.to_string(),
            address: address.to_string(),
            port,
            role,
            last_heartbeat_ms: self.current_time_ms,
            metadata: HashMap::new(),
        };
        self.nodes.insert(node_id.to_string(), info);
    }

    /// Record a heartbeat from a node.
    pub fn heartbeat(&mut self, node_id: &str) -> bool {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.last_heartbeat_ms = self.current_time_ms;
            true
        } else {
            false
        }
    }

    /// Get the status of a node based on heartbeat freshness.
    pub fn node_status(&self, node_id: &str) -> Option<NodeStatus> {
        self.nodes.get(node_id).map(|n| {
            if n.last_heartbeat_ms == self.current_time_ms {
                NodeStatus::Healthy
            } else if self.current_time_ms.saturating_sub(n.last_heartbeat_ms) <= self.heartbeat_timeout_ms {
                NodeStatus::Suspect
            } else {
                NodeStatus::Dead
            }
        })
    }

    /// Deregister a node.
    pub fn deregister(&mut self, node_id: &str) -> bool {
        self.nodes.remove(node_id).is_some()
    }

    /// Get all nodes with a specific role.
    pub fn nodes_by_role(&self, role: &NodeRole) -> Vec<&NodeInfo> {
        self.nodes.values().filter(|n| &n.role == role).collect()
    }

    /// Get all healthy nodes.
    pub fn healthy_nodes(&self) -> Vec<&NodeInfo> {
        self.nodes.values().filter(|n| {
            self.current_time_ms.saturating_sub(n.last_heartbeat_ms) <= self.heartbeat_timeout_ms
        }).collect()
    }

    /// Set metadata on a node.
    pub fn set_metadata(&mut self, node_id: &str, key: &str, value: &str) -> bool {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.metadata.insert(key.to_string(), value.to_string());
            true
        } else {
            false
        }
    }

    /// Get the total number of registered nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

// ─── Configuration Center ────────────────────────────────────────────

/// A versioned configuration entry.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
    pub version: u64,
    pub updated_at_ms: u64,
    pub source: ConfigSource,
}

/// Where a configuration came from.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigSource {
    Default,
    Local,
    Remote,
    Override,
}

/// Distributed configuration management with versioning and namespaces.
pub struct ConfigCenter {
    entries: HashMap<String, ConfigEntry>,
    namespaces: HashMap<String, Vec<String>>,
    watchers: HashMap<String, Vec<String>>,
    global_version: u64,
    current_time_ms: u64,
}

impl ConfigCenter {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            namespaces: HashMap::new(),
            watchers: HashMap::new(),
            global_version: 0,
            current_time_ms: 0,
        }
    }

    /// Set the current time.
    pub fn set_time(&mut self, now_ms: u64) {
        self.current_time_ms = now_ms;
    }

    /// Put a configuration entry, incrementing global version.
    pub fn put(&mut self, namespace: &str, key: &str, value: &str, source: ConfigSource) -> u64 {
        self.global_version += 1;
        let full_key = format!("{}/{}", namespace, key);
        let entry = ConfigEntry {
            key: full_key.clone(),
            value: value.to_string(),
            version: self.global_version,
            updated_at_ms: self.current_time_ms,
            source,
        };
        self.entries.insert(full_key.clone(), entry);
        self.namespaces.entry(namespace.to_string()).or_default().push(full_key);
        self.global_version
    }

    /// Get a configuration value by namespace and key.
    pub fn get(&self, namespace: &str, key: &str) -> Option<&ConfigEntry> {
        let full_key = format!("{}/{}", namespace, key);
        self.entries.get(&full_key)
    }

    /// Delete a configuration entry.
    pub fn delete(&mut self, namespace: &str, key: &str) -> bool {
        let full_key = format!("{}/{}", namespace, key);
        if self.entries.remove(&full_key).is_some() {
            if let Some(keys) = self.namespaces.get_mut(namespace) {
                keys.retain(|k| k != &full_key);
            }
            true
        } else {
            false
        }
    }

    /// List all keys in a namespace.
    pub fn list_namespace(&self, namespace: &str) -> Vec<&ConfigEntry> {
        self.namespaces.get(namespace)
            .map(|keys| {
                keys.iter()
                    .filter_map(|k| self.entries.get(k))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Register a watcher for a key.
    pub fn watch(&mut self, namespace: &str, key: &str, watcher_id: &str) {
        let full_key = format!("{}/{}", namespace, key);
        self.watchers.entry(full_key).or_default().push(watcher_id.to_string());
    }

    /// Get watchers for a key.
    pub fn get_watchers(&self, namespace: &str, key: &str) -> Vec<&str> {
        let full_key = format!("{}/{}", namespace, key);
        self.watchers.get(&full_key)
            .map(|ws| ws.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get the current global version.
    pub fn version(&self) -> u64 {
        self.global_version
    }

    /// Get total number of config entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Snapshot all entries whose version > since_version.
    pub fn changes_since(&self, since_version: u64) -> Vec<&ConfigEntry> {
        self.entries.values()
            .filter(|e| e.version > since_version)
            .collect()
    }
}

// ─── Service Mesh ────────────────────────────────────────────────────

/// A service endpoint in the mesh.
#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub service_name: String,
    pub node_id: String,
    pub address: String,
    pub port: u16,
    pub weight: u32,
    pub healthy: bool,
}

/// Routing strategy for a service.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutingStrategy {
    RoundRobin,
    WeightedRandom,
    LeastConnections,
    Consistent,
}

/// Service mesh with registration, routing, and health tracking.
pub struct ServiceMesh {
    services: HashMap<String, Vec<ServiceEndpoint>>,
    routing: HashMap<String, RoutingStrategy>,
    call_counts: HashMap<String, u64>,
    round_robin_idx: HashMap<String, usize>,
}

impl ServiceMesh {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            routing: HashMap::new(),
            call_counts: HashMap::new(),
            round_robin_idx: HashMap::new(),
        }
    }

    /// Register a service endpoint.
    pub fn register_service(
        &mut self,
        service_name: &str,
        node_id: &str,
        address: &str,
        port: u16,
        weight: u32,
    ) {
        let ep = ServiceEndpoint {
            service_name: service_name.to_string(),
            node_id: node_id.to_string(),
            address: address.to_string(),
            port,
            weight,
            healthy: true,
        };
        self.services.entry(service_name.to_string()).or_default().push(ep);
    }

    /// Set the routing strategy for a service.
    pub fn set_routing(&mut self, service_name: &str, strategy: RoutingStrategy) {
        self.routing.insert(service_name.to_string(), strategy);
    }

    /// Resolve a service to one endpoint using the configured routing strategy.
    pub fn resolve(&mut self, service_name: &str) -> Option<&ServiceEndpoint> {
        let strategy = self.routing.get(service_name)
            .cloned()
            .unwrap_or(RoutingStrategy::RoundRobin);

        let endpoints = self.services.get(service_name)?;
        let healthy: Vec<usize> = endpoints.iter().enumerate()
            .filter(|(_, ep)| ep.healthy)
            .map(|(i, _)| i)
            .collect();

        if healthy.is_empty() {
            return None;
        }

        let chosen_idx = match strategy {
            RoutingStrategy::RoundRobin => {
                let idx = self.round_robin_idx.entry(service_name.to_string()).or_insert(0);
                let pos = *idx % healthy.len();
                *idx = idx.wrapping_add(1);
                healthy[pos]
            }
            RoutingStrategy::WeightedRandom => {
                // Use deterministic weight-based selection: pick highest weight
                let max_w = healthy.iter()
                    .map(|&i| endpoints[i].weight)
                    .max()
                    .unwrap_or(1);
                *healthy.iter().find(|&&i| endpoints[i].weight == max_w).unwrap()
            }
            RoutingStrategy::LeastConnections => {
                // Pick the endpoint with fewest recorded calls
                let min_calls_idx = healthy.iter()
                    .min_by_key(|&&i| {
                        let key = format!("{}:{}", service_name, endpoints[i].node_id);
                        self.call_counts.get(&key).copied().unwrap_or(0)
                    })
                    .copied()
                    .unwrap();
                min_calls_idx
            }
            RoutingStrategy::Consistent => {
                healthy[0] // stable assignment
            }
        };

        // Track calls
        let node_key = format!("{}:{}", service_name, endpoints[chosen_idx].node_id);
        *self.call_counts.entry(node_key).or_insert(0) += 1;

        Some(&self.services.get(service_name).unwrap()[chosen_idx])
    }

    /// Mark an endpoint as unhealthy.
    pub fn mark_unhealthy(&mut self, service_name: &str, node_id: &str) -> bool {
        if let Some(endpoints) = self.services.get_mut(service_name) {
            for ep in endpoints.iter_mut() {
                if ep.node_id == node_id {
                    ep.healthy = false;
                    return true;
                }
            }
        }
        false
    }

    /// Mark an endpoint as healthy.
    pub fn mark_healthy(&mut self, service_name: &str, node_id: &str) -> bool {
        if let Some(endpoints) = self.services.get_mut(service_name) {
            for ep in endpoints.iter_mut() {
                if ep.node_id == node_id {
                    ep.healthy = true;
                    return true;
                }
            }
        }
        false
    }

    /// Get the number of healthy endpoints for a service.
    pub fn healthy_count(&self, service_name: &str) -> usize {
        self.services.get(service_name)
            .map(|eps| eps.iter().filter(|e| e.healthy).count())
            .unwrap_or(0)
    }

    /// Get total registered services.
    pub fn service_count(&self) -> usize {
        self.services.len()
    }
}

// ─── Link Encryption ────────────────────────────────────────────────

/// Encryption algorithm for inter-node communication.
#[derive(Debug, Clone, PartialEq)]
pub enum EncryptionAlgo {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
    None,
}

/// A session key for encrypted communication.
#[derive(Debug, Clone)]
pub struct SessionKey {
    pub key_id: String,
    pub algorithm: EncryptionAlgo,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub key_material: Vec<u8>,
}

/// TLS/encryption certificate info.
#[derive(Debug, Clone)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before_ms: u64,
    pub not_after_ms: u64,
    pub fingerprint: String,
}

/// Link encryption manager for inter-node communication.
pub struct LinkEncryption {
    session_keys: HashMap<String, SessionKey>,
    certificates: HashMap<String, CertInfo>,
    default_algo: EncryptionAlgo,
    rotation_interval_ms: u64,
    current_time_ms: u64,
}

impl LinkEncryption {
    pub fn new(default_algo: EncryptionAlgo, rotation_interval_ms: u64) -> Self {
        Self {
            session_keys: HashMap::new(),
            certificates: HashMap::new(),
            default_algo,
            rotation_interval_ms,
            current_time_ms: 0,
        }
    }

    /// Set the current time.
    pub fn set_time(&mut self, now_ms: u64) {
        self.current_time_ms = now_ms;
    }

    /// Create a session key for a peer.
    pub fn create_session_key(&mut self, peer_id: &str) -> &SessionKey {
        let key = SessionKey {
            key_id: format!("sk-{}-{}", peer_id, self.current_time_ms),
            algorithm: self.default_algo.clone(),
            created_at_ms: self.current_time_ms,
            expires_at_ms: self.current_time_ms + self.rotation_interval_ms,
            key_material: vec![0xAB; 32], // simulated key material
        };
        self.session_keys.insert(peer_id.to_string(), key);
        self.session_keys.get(peer_id).unwrap()
    }

    /// Check if a peer's session key is still valid.
    pub fn is_key_valid(&self, peer_id: &str) -> bool {
        self.session_keys.get(peer_id)
            .map(|k| self.current_time_ms < k.expires_at_ms)
            .unwrap_or(false)
    }

    /// Rotate the session key for a peer.
    pub fn rotate_key(&mut self, peer_id: &str) -> bool {
        if self.session_keys.contains_key(peer_id) {
            self.create_session_key(peer_id);
            true
        } else {
            false
        }
    }

    /// Find all expired keys that need rotation.
    pub fn expired_keys(&self) -> Vec<&str> {
        self.session_keys.iter()
            .filter(|(_, k)| self.current_time_ms >= k.expires_at_ms)
            .map(|(peer, _)| peer.as_str())
            .collect()
    }

    /// Register a certificate for a node.
    pub fn register_cert(&mut self, node_id: &str, cert: CertInfo) {
        self.certificates.insert(node_id.to_string(), cert);
    }

    /// Get certificate info for a node.
    pub fn get_cert(&self, node_id: &str) -> Option<&CertInfo> {
        self.certificates.get(node_id)
    }

    /// Check if a certificate is still valid.
    pub fn is_cert_valid(&self, node_id: &str) -> bool {
        self.certificates.get(node_id)
            .map(|c| self.current_time_ms >= c.not_before_ms && self.current_time_ms < c.not_after_ms)
            .unwrap_or(false)
    }

    /// Encrypt a payload (simulated — XOR with key material).
    pub fn encrypt(&self, peer_id: &str, plaintext: &[u8]) -> Option<Vec<u8>> {
        let key = self.session_keys.get(peer_id)?;
        if self.current_time_ms >= key.expires_at_ms {
            return None; // expired
        }
        let encrypted: Vec<u8> = plaintext.iter().enumerate()
            .map(|(i, &b)| b ^ key.key_material[i % key.key_material.len()])
            .collect();
        Some(encrypted)
    }

    /// Decrypt a payload (simulated — XOR is symmetric).
    pub fn decrypt(&self, peer_id: &str, ciphertext: &[u8]) -> Option<Vec<u8>> {
        // XOR is symmetric, so encrypt == decrypt
        self.encrypt(peer_id, ciphertext)
    }

    /// Get total number of active session keys.
    pub fn active_key_count(&self) -> usize {
        self.session_keys.values()
            .filter(|k| self.current_time_ms < k.expires_at_ms)
            .count()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_discovery_register_and_heartbeat() {
        let mut nd = NodeDiscovery::new(5000);
        nd.tick(1000);
        nd.register("n1", "10.0.0.1", 8080, NodeRole::Leader);
        nd.register("n2", "10.0.0.2", 8080, NodeRole::Follower);
        assert_eq!(nd.node_count(), 2);
        assert_eq!(nd.node_status("n1"), Some(NodeStatus::Healthy));

        nd.tick(3000);
        // n1 no heartbeat since 1000, but within 5000ms window
        assert_eq!(nd.node_status("n1"), Some(NodeStatus::Suspect));

        nd.heartbeat("n1");
        assert_eq!(nd.node_status("n1"), Some(NodeStatus::Healthy));
    }

    #[test]
    fn test_node_discovery_dead_detection() {
        let mut nd = NodeDiscovery::new(2000);
        nd.tick(1000);
        nd.register("n1", "10.0.0.1", 8080, NodeRole::Follower);
        nd.tick(4000); // 3000ms since last heartbeat > 2000ms timeout
        assert_eq!(nd.node_status("n1"), Some(NodeStatus::Dead));
    }

    #[test]
    fn test_node_discovery_roles_and_deregister() {
        let mut nd = NodeDiscovery::new(5000);
        nd.tick(0);
        nd.register("n1", "10.0.0.1", 8080, NodeRole::Leader);
        nd.register("n2", "10.0.0.2", 8080, NodeRole::Observer);
        nd.register("n3", "10.0.0.3", 8080, NodeRole::Observer);
        assert_eq!(nd.nodes_by_role(&NodeRole::Observer).len(), 2);
        assert!(nd.deregister("n3"));
        assert_eq!(nd.nodes_by_role(&NodeRole::Observer).len(), 1);
        assert!(!nd.deregister("n99"));
    }

    #[test]
    fn test_config_center_put_get_delete() {
        let mut cc = ConfigCenter::new();
        cc.set_time(1000);
        let v1 = cc.put("db", "max_connections", "100", ConfigSource::Default);
        assert_eq!(v1, 1);
        let entry = cc.get("db", "max_connections").unwrap();
        assert_eq!(entry.value, "100");
        assert_eq!(entry.source, ConfigSource::Default);

        cc.put("db", "timeout", "30", ConfigSource::Local);
        assert_eq!(cc.entry_count(), 2);

        assert!(cc.delete("db", "timeout"));
        assert_eq!(cc.entry_count(), 1);
        assert!(!cc.delete("db", "nonexistent"));
    }

    #[test]
    fn test_config_center_namespace_and_watchers() {
        let mut cc = ConfigCenter::new();
        cc.put("app", "key1", "val1", ConfigSource::Remote);
        cc.put("app", "key2", "val2", ConfigSource::Remote);
        cc.put("sys", "key3", "val3", ConfigSource::Override);
        assert_eq!(cc.list_namespace("app").len(), 2);
        assert_eq!(cc.list_namespace("sys").len(), 1);

        cc.watch("app", "key1", "watcher-A");
        cc.watch("app", "key1", "watcher-B");
        assert_eq!(cc.get_watchers("app", "key1").len(), 2);
        assert_eq!(cc.get_watchers("app", "key2").len(), 0);
    }

    #[test]
    fn test_config_center_changes_since() {
        let mut cc = ConfigCenter::new();
        cc.put("ns", "a", "1", ConfigSource::Default);
        cc.put("ns", "b", "2", ConfigSource::Default);
        let v = cc.version();
        cc.put("ns", "c", "3", ConfigSource::Local);
        let changes = cc.changes_since(v);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].value, "3");
    }

    #[test]
    fn test_service_mesh_registration_and_routing() {
        let mut mesh = ServiceMesh::new();
        mesh.register_service("query", "n1", "10.0.0.1", 9000, 10);
        mesh.register_service("query", "n2", "10.0.0.2", 9000, 20);
        mesh.set_routing("query", RoutingStrategy::RoundRobin);

        let ep1 = mesh.resolve("query").unwrap();
        assert_eq!(ep1.node_id, "n1");
        let ep2 = mesh.resolve("query").unwrap();
        assert_eq!(ep2.node_id, "n2");
        // wraps around
        let ep3 = mesh.resolve("query").unwrap();
        assert_eq!(ep3.node_id, "n1");
    }

    #[test]
    fn test_service_mesh_unhealthy() {
        let mut mesh = ServiceMesh::new();
        mesh.register_service("store", "n1", "10.0.0.1", 9000, 10);
        mesh.register_service("store", "n2", "10.0.0.2", 9000, 10);
        assert_eq!(mesh.healthy_count("store"), 2);

        mesh.mark_unhealthy("store", "n1");
        assert_eq!(mesh.healthy_count("store"), 1);

        let ep = mesh.resolve("store").unwrap();
        assert_eq!(ep.node_id, "n2"); // only n2 is healthy

        mesh.mark_healthy("store", "n1");
        assert_eq!(mesh.healthy_count("store"), 2);
    }

    #[test]
    fn test_link_encryption_session_keys() {
        let mut le = LinkEncryption::new(EncryptionAlgo::Aes256Gcm, 10000);
        le.set_time(1000);
        le.create_session_key("peer-1");
        assert!(le.is_key_valid("peer-1"));
        assert!(!le.is_key_valid("peer-unknown"));

        le.set_time(12000); // after 10000ms rotation interval
        assert!(!le.is_key_valid("peer-1"));
        let expired = le.expired_keys();
        assert_eq!(expired.len(), 1);
    }

    #[test]
    fn test_link_encryption_encrypt_decrypt() {
        let mut le = LinkEncryption::new(EncryptionAlgo::ChaCha20Poly1305, 50000);
        le.set_time(1000);
        le.create_session_key("peer-1");

        let plaintext = b"hello distributed world";
        let encrypted = le.encrypt("peer-1", plaintext).unwrap();
        assert_ne!(&encrypted, plaintext);

        let decrypted = le.decrypt("peer-1", &encrypted).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn test_link_encryption_cert_management() {
        let mut le = LinkEncryption::new(EncryptionAlgo::Aes128Gcm, 10000);
        le.set_time(5000);
        le.register_cert("n1", CertInfo {
            subject: "n1.cluster.local".into(),
            issuer: "ca.cluster.local".into(),
            serial: "0001".into(),
            not_before_ms: 1000,
            not_after_ms: 100000,
            fingerprint: "abc123".into(),
        });
        assert!(le.is_cert_valid("n1"));

        le.set_time(200000);
        assert!(!le.is_cert_valid("n1")); // expired

        assert!(!le.is_cert_valid("n99")); // non-existent
    }
}
