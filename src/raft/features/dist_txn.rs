// R16 – Distributed transactions & consistency: two-phase lock upgrade,
//       MVCC global serialization, distributed DDL coordination,
//       multi-version schema management.
//
// Provides:
//   - `LockUpgradeManager`: S→X lock upgrade with deadlock avoidance
//   - `GlobalSerializer`: global commit ordering for serializable MVCC
//   - `DistributedDdlCoordinator`: multi-node DDL coordination state machine
//   - `SchemaVersionManager`: multi-version schema with compatibility checks

use std::collections::{HashMap, HashSet};

// ── Two-Phase Lock Upgrade ────────────────────────────────────────────

/// Lock modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockMode {
    Shared,
    Update, // intention to upgrade
    Exclusive,
}

impl LockMode {
    /// Check if this mode is compatible with another.
    pub fn is_compatible(&self, other: &LockMode) -> bool {
        matches!(
            (self, other),
            (LockMode::Shared, LockMode::Shared)
                | (LockMode::Shared, LockMode::Update)
                | (LockMode::Update, LockMode::Shared)
        )
    }
}

/// A lock held by a transaction.
#[derive(Debug, Clone)]
pub struct HeldLock {
    pub txn_id: u64,
    pub resource: String,
    pub mode: LockMode,
}

/// Manages S→X lock upgrades with deadlock avoidance.
pub struct LockUpgradeManager {
    locks: Vec<HeldLock>,
    upgrade_queue: Vec<(u64, String)>, // (txn_id, resource) waiting for upgrade
}

impl Default for LockUpgradeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LockUpgradeManager {
    pub fn new() -> Self {
        Self {
            locks: Vec::new(),
            upgrade_queue: Vec::new(),
        }
    }

    /// Acquire a lock.
    pub fn acquire(&mut self, txn_id: u64, resource: &str, mode: LockMode) -> bool {
        // Check compatibility with existing locks from other txns
        for lock in &self.locks {
            if lock.resource == resource && lock.txn_id != txn_id && !mode.is_compatible(&lock.mode)
            {
                return false;
            }
        }
        self.locks.push(HeldLock {
            txn_id,
            resource: resource.to_string(),
            mode,
        });
        true
    }

    /// Upgrade a lock from Shared to Exclusive.
    pub fn upgrade(&mut self, txn_id: u64, resource: &str) -> bool {
        // Check if txn holds a shared lock
        let has_shared = self
            .locks
            .iter()
            .any(|l| l.txn_id == txn_id && l.resource == resource && l.mode == LockMode::Shared);
        if !has_shared {
            return false;
        }

        // Check if any OTHER txn holds a lock on this resource
        let others_hold = self
            .locks
            .iter()
            .any(|l| l.resource == resource && l.txn_id != txn_id);
        if others_hold {
            self.upgrade_queue.push((txn_id, resource.to_string()));
            return false;
        }

        // Upgrade in-place
        for lock in &mut self.locks {
            if lock.txn_id == txn_id && lock.resource == resource {
                lock.mode = LockMode::Exclusive;
                return true;
            }
        }
        false
    }

    /// Release all locks for a transaction.
    pub fn release(&mut self, txn_id: u64) {
        self.locks.retain(|l| l.txn_id != txn_id);
        self.upgrade_queue.retain(|(tid, _)| *tid != txn_id);
    }

    pub fn lock_count(&self) -> usize {
        self.locks.len()
    }

    pub fn upgrade_queue_len(&self) -> usize {
        self.upgrade_queue.len()
    }

    /// Check if a txn holds a specific lock mode.
    pub fn holds_lock(&self, txn_id: u64, resource: &str, mode: LockMode) -> bool {
        self.locks
            .iter()
            .any(|l| l.txn_id == txn_id && l.resource == resource && l.mode == mode)
    }
}

// ── Global Serializer ─────────────────────────────────────────────────

/// Commit ordering entry.
#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub txn_id: u64,
    pub commit_ts: u64,
    pub read_set: HashSet<String>,
    pub write_set: HashSet<String>,
}

/// Ensures global serializable ordering across MVCC transactions.
pub struct GlobalSerializer {
    committed: Vec<CommitEntry>,
    next_ts: u64,
}

impl Default for GlobalSerializer {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalSerializer {
    pub fn new() -> Self {
        Self {
            committed: Vec::new(),
            next_ts: 1,
        }
    }

    /// Allocate a commit timestamp.
    pub fn allocate_ts(&mut self) -> u64 {
        let ts = self.next_ts;
        self.next_ts += 1;
        ts
    }

    /// Validate that a transaction can commit without violating serializability.
    pub fn validate(
        &self,
        _txn_id: u64,
        read_set: &HashSet<String>,
        write_set: &HashSet<String>,
        begin_ts: u64,
    ) -> bool {
        // Check for write-write and read-write conflicts with concurrent txns
        for entry in &self.committed {
            if entry.commit_ts <= begin_ts {
                continue; // committed before txn started
            }
            // Check if committed txn wrote something we read
            if !entry.write_set.is_disjoint(read_set) {
                return false;
            }
            // Check if committed txn wrote something we also wrote
            if !entry.write_set.is_disjoint(write_set) {
                return false;
            }
        }
        true
    }

    /// Record a committed transaction.
    pub fn commit(
        &mut self,
        txn_id: u64,
        read_set: HashSet<String>,
        write_set: HashSet<String>,
    ) -> u64 {
        let ts = self.allocate_ts();
        self.committed.push(CommitEntry {
            txn_id,
            commit_ts: ts,
            read_set,
            write_set,
        });
        ts
    }

    pub fn committed_count(&self) -> usize {
        self.committed.len()
    }

    pub fn current_ts(&self) -> u64 {
        self.next_ts - 1
    }
}

// ── Distributed DDL Coordinator ───────────────────────────────────────

/// DDL coordination phases.
#[derive(Debug, Clone, PartialEq)]
pub enum DdlPhase {
    Propose,
    Prepare,
    Execute,
    Commit,
    Rollback,
    Completed,
}

/// A distributed DDL operation.
#[derive(Debug, Clone)]
pub struct DdlOperation {
    pub op_id: u64,
    pub sql: String,
    pub phase: DdlPhase,
    pub participating_nodes: HashSet<u64>,
    pub acks: HashSet<u64>,
}

/// Coordinates DDL operations across multiple nodes.
pub struct DistributedDdlCoordinator {
    operations: HashMap<u64, DdlOperation>,
    next_id: u64,
}

impl Default for DistributedDdlCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl DistributedDdlCoordinator {
    pub fn new() -> Self {
        Self {
            operations: HashMap::new(),
            next_id: 1,
        }
    }

    /// Propose a DDL operation.
    pub fn propose(&mut self, sql: &str, nodes: HashSet<u64>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.operations.insert(
            id,
            DdlOperation {
                op_id: id,
                sql: sql.to_string(),
                phase: DdlPhase::Propose,
                participating_nodes: nodes,
                acks: HashSet::new(),
            },
        );
        id
    }

    /// Receive a prepare ack from a node.
    pub fn receive_ack(&mut self, op_id: u64, node_id: u64) -> Option<DdlPhase> {
        if let Some(op) = self.operations.get_mut(&op_id) {
            if op.participating_nodes.contains(&node_id) {
                op.acks.insert(node_id);
            }
            // If all nodes ack'd, advance phase
            if op.acks.len() == op.participating_nodes.len() {
                let next_phase = match op.phase {
                    DdlPhase::Propose => DdlPhase::Prepare,
                    DdlPhase::Prepare => DdlPhase::Execute,
                    DdlPhase::Execute => DdlPhase::Commit,
                    DdlPhase::Commit => DdlPhase::Completed,
                    _ => op.phase.clone(),
                };
                op.phase = next_phase.clone();
                op.acks.clear();
                Some(next_phase)
            } else {
                Some(op.phase.clone())
            }
        } else {
            None
        }
    }

    /// Force rollback a DDL operation.
    pub fn rollback(&mut self, op_id: u64) -> bool {
        if let Some(op) = self.operations.get_mut(&op_id) {
            op.phase = DdlPhase::Rollback;
            true
        } else {
            false
        }
    }

    pub fn phase(&self, op_id: u64) -> Option<&DdlPhase> {
        self.operations.get(&op_id).map(|o| &o.phase)
    }

    pub fn active_operations(&self) -> usize {
        self.operations
            .values()
            .filter(|o| o.phase != DdlPhase::Completed && o.phase != DdlPhase::Rollback)
            .count()
    }
}

// ── Multi-Version Schema Manager ──────────────────────────────────────

/// A schema version.
#[derive(Debug, Clone)]
pub struct SchemaVersion {
    pub version: u64,
    pub columns: Vec<(String, String)>, // (name, type)
    pub created_at: u64,
    pub is_active: bool,
}

/// Compatibility check result.
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaCompat {
    Compatible,
    AddedColumns(Vec<String>),
    DroppedColumns(Vec<String>),
    Incompatible(String),
}

/// Manages multiple schema versions for online schema evolution.
pub struct SchemaVersionManager {
    versions: HashMap<String, Vec<SchemaVersion>>, // table → versions
    next_version: u64,
}

impl Default for SchemaVersionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaVersionManager {
    pub fn new() -> Self {
        Self {
            versions: HashMap::new(),
            next_version: 1,
        }
    }

    /// Register a new schema version for a table.
    pub fn add_version(
        &mut self,
        table: &str,
        columns: Vec<(String, String)>,
        timestamp: u64,
    ) -> u64 {
        let ver = self.next_version;
        self.next_version += 1;
        let v = SchemaVersion {
            version: ver,
            columns,
            created_at: timestamp,
            is_active: true,
        };
        // Deactivate previous versions
        if let Some(versions) = self.versions.get_mut(table) {
            for old in versions.iter_mut() {
                old.is_active = false;
            }
            versions.push(v);
        } else {
            self.versions.insert(table.to_string(), vec![v]);
        }
        ver
    }

    /// Get active schema version for a table.
    pub fn active_version(&self, table: &str) -> Option<&SchemaVersion> {
        self.versions
            .get(table)
            .and_then(|vs| vs.iter().rfind(|v| v.is_active))
    }

    /// Check compatibility between two versions.
    pub fn check_compat(&self, table: &str, v1: u64, v2: u64) -> SchemaCompat {
        let versions = match self.versions.get(table) {
            Some(vs) => vs,
            None => return SchemaCompat::Incompatible("table not found".to_string()),
        };
        let schema_v1 = match versions.iter().find(|v| v.version == v1) {
            Some(v) => v,
            None => return SchemaCompat::Incompatible("v1 not found".to_string()),
        };
        let schema_v2 = match versions.iter().find(|v| v.version == v2) {
            Some(v) => v,
            None => return SchemaCompat::Incompatible("v2 not found".to_string()),
        };

        let v1_cols: HashSet<&str> = schema_v1.columns.iter().map(|(n, _)| n.as_str()).collect();
        let v2_cols: HashSet<&str> = schema_v2.columns.iter().map(|(n, _)| n.as_str()).collect();

        let added: Vec<String> = v2_cols
            .difference(&v1_cols)
            .map(|s| s.to_string())
            .collect();
        let dropped: Vec<String> = v1_cols
            .difference(&v2_cols)
            .map(|s| s.to_string())
            .collect();

        if added.is_empty() && dropped.is_empty() {
            SchemaCompat::Compatible
        } else if !added.is_empty() && dropped.is_empty() {
            SchemaCompat::AddedColumns(added)
        } else if added.is_empty() && !dropped.is_empty() {
            SchemaCompat::DroppedColumns(dropped)
        } else {
            SchemaCompat::Incompatible(format!("added {:?}, dropped {:?}", added, dropped))
        }
    }

    pub fn version_count(&self, table: &str) -> usize {
        self.versions.get(table).map(|vs| vs.len()).unwrap_or(0)
    }

    pub fn table_count(&self) -> usize {
        self.versions.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_upgrade_basic() {
        let mut lm = LockUpgradeManager::new();
        assert!(lm.acquire(1, "table_a", LockMode::Shared));
        assert!(lm.acquire(2, "table_a", LockMode::Shared)); // compatible
        assert!(!lm.upgrade(1, "table_a")); // can't upgrade, txn2 holds shared
        lm.release(2);
        assert!(lm.upgrade(1, "table_a"));
        assert!(lm.holds_lock(1, "table_a", LockMode::Exclusive));
    }

    #[test]
    fn lock_upgrade_exclusive_blocks() {
        let mut lm = LockUpgradeManager::new();
        assert!(lm.acquire(1, "r1", LockMode::Exclusive));
        assert!(!lm.acquire(2, "r1", LockMode::Shared)); // blocked
    }

    #[test]
    fn global_serializer_validate() {
        let mut gs = GlobalSerializer::new();
        let begin_ts = gs.allocate_ts();

        // Txn 1 commits, writing to "x"
        let ws1: HashSet<String> = vec!["x".to_string()].into_iter().collect();
        gs.commit(1, HashSet::new(), ws1);

        // Txn 2 tries to commit, having read "x" (started before txn1 committed)
        let rs2: HashSet<String> = vec!["x".to_string()].into_iter().collect();
        assert!(!gs.validate(2, &rs2, &HashSet::new(), begin_ts));
    }

    #[test]
    fn global_serializer_no_conflict() {
        let mut gs = GlobalSerializer::new();
        let begin_ts = gs.allocate_ts();
        let ws1: HashSet<String> = vec!["x".to_string()].into_iter().collect();
        gs.commit(1, HashSet::new(), ws1);

        // Txn 2 only reads "y" → no conflict
        let rs2: HashSet<String> = vec!["y".to_string()].into_iter().collect();
        assert!(gs.validate(2, &rs2, &HashSet::new(), begin_ts));
    }

    #[test]
    fn distributed_ddl_coordination() {
        let mut coord = DistributedDdlCoordinator::new();
        let nodes: HashSet<u64> = vec![1, 2, 3].into_iter().collect();
        let op = coord.propose("ALTER TABLE t ADD col INT", nodes);

        assert_eq!(coord.phase(op), Some(&DdlPhase::Propose));
        coord.receive_ack(op, 1);
        coord.receive_ack(op, 2);
        let phase = coord.receive_ack(op, 3).unwrap();
        assert_eq!(phase, DdlPhase::Prepare); // all ack'd → advance
    }

    #[test]
    fn distributed_ddl_rollback() {
        let mut coord = DistributedDdlCoordinator::new();
        let nodes: HashSet<u64> = vec![1, 2].into_iter().collect();
        let op = coord.propose("DROP TABLE t", nodes);
        assert!(coord.rollback(op));
        assert_eq!(coord.phase(op), Some(&DdlPhase::Rollback));
        assert_eq!(coord.active_operations(), 0);
    }

    #[test]
    fn schema_version_manager_add() {
        let mut svm = SchemaVersionManager::new();
        let _v1 = svm.add_version(
            "users",
            vec![
                ("id".to_string(), "INT".to_string()),
                ("name".to_string(), "TEXT".to_string()),
            ],
            1,
        );
        let v2 = svm.add_version(
            "users",
            vec![
                ("id".to_string(), "INT".to_string()),
                ("name".to_string(), "TEXT".to_string()),
                ("email".to_string(), "TEXT".to_string()),
            ],
            2,
        );

        assert_eq!(svm.version_count("users"), 2);
        let active = svm.active_version("users").unwrap();
        assert_eq!(active.version, v2);
        assert_eq!(active.columns.len(), 3);
    }

    #[test]
    fn schema_version_compat() {
        let mut svm = SchemaVersionManager::new();
        let v1 = svm.add_version("t", vec![("a".to_string(), "INT".to_string())], 1);
        let v2 = svm.add_version(
            "t",
            vec![
                ("a".to_string(), "INT".to_string()),
                ("b".to_string(), "TEXT".to_string()),
            ],
            2,
        );
        match svm.check_compat("t", v1, v2) {
            SchemaCompat::AddedColumns(cols) => assert!(cols.contains(&"b".to_string())),
            other => panic!("expected AddedColumns, got {:?}", other),
        }
    }
}
