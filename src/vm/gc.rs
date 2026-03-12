// R13 – Data integrity enhancement: MVCC garbage collection, isolation
//       level verification helpers, foreign-key cascade tracking.
//
// Provides:
//   - `MvccGarbageCollector`: purges old row versions below a watermark
//   - `IsolationLevel` + `IsolationVerifier`: validates read/write sets
//   - `ForeignKeyCascade`: tracks cascade relationships and generates ops

use std::collections::{HashMap, HashSet};
use std::time::Instant;

// ── MVCC Garbage Collector ────────────────────────────────────────────

/// A single versioned row.
#[derive(Debug, Clone)]
pub struct VersionedRow {
    pub row_id: u64,
    pub txn_id: u64,
    pub data: Vec<u8>, // opaque payload (serialized row)
    pub deleted: bool,
    pub created_at: Instant,
}

impl VersionedRow {
    pub fn new(row_id: u64, txn_id: u64, data: Vec<u8>) -> Self {
        Self {
            row_id,
            txn_id,
            data,
            deleted: false,
            created_at: Instant::now(),
        }
    }

    pub fn new_deleted(row_id: u64, txn_id: u64) -> Self {
        Self {
            row_id,
            txn_id,
            data: Vec::new(),
            deleted: true,
            created_at: Instant::now(),
        }
    }
}

/// MVCC garbage collector that purges old row versions.
pub struct MvccGarbageCollector {
    /// row_id → list of versions ordered by txn_id ascending.
    versions: HashMap<u64, Vec<VersionedRow>>,
    /// The current safe watermark: versions with txn_id below this can be purged.
    watermark: u64,
    /// Total bytes purged since last reset.
    bytes_purged: u64,
    /// Total versions purged since last reset.
    versions_purged: u64,
}

impl MvccGarbageCollector {
    pub fn new(initial_watermark: u64) -> Self {
        Self {
            versions: HashMap::new(),
            watermark: initial_watermark,
            bytes_purged: 0,
            versions_purged: 0,
        }
    }

    /// Add a version for a row.
    pub fn add_version(&mut self, ver: VersionedRow) {
        let entry = self.versions.entry(ver.row_id).or_default();
        entry.push(ver);
    }

    /// Advance the watermark (only forward).
    pub fn advance_watermark(&mut self, new_watermark: u64) {
        if new_watermark > self.watermark {
            self.watermark = new_watermark;
        }
    }

    pub fn watermark(&self) -> u64 {
        self.watermark
    }

    /// Purge versions below the watermark. For each row, keep the latest
    /// version that is ≤ watermark (we need it for reads) and discard older ones.
    pub fn purge(&mut self) -> u64 {
        let mut total_purged = 0u64;
        for versions in self.versions.values_mut() {
            if versions.len() <= 1 {
                continue;
            }
            // Find the latest version ≤ watermark
            let cutoff_idx = versions
                .iter()
                .rposition(|v| v.txn_id <= self.watermark);
            if let Some(idx) = cutoff_idx {
                if idx == 0 {
                    continue; // nothing to purge
                }
                // Remove versions [0..idx), keep [idx..]
                let removed: Vec<_> = versions.drain(0..idx).collect();
                for r in &removed {
                    self.bytes_purged += r.data.len() as u64;
                }
                total_purged += removed.len() as u64;
            }
        }
        self.versions_purged += total_purged;
        total_purged
    }

    /// Remove all versions for rows that have been deleted and are below watermark.
    pub fn purge_tombstones(&mut self) -> u64 {
        let mut purged = 0u64;
        self.versions.retain(|_row_id, versions| {
            if versions.len() == 1
                && versions[0].deleted
                && versions[0].txn_id <= self.watermark
            {
                purged += 1;
                false // remove
            } else {
                true
            }
        });
        self.versions_purged += purged;
        purged
    }

    /// Count of row-id entries.
    pub fn row_count(&self) -> usize {
        self.versions.len()
    }

    /// Total versions across all rows.
    pub fn version_count(&self) -> usize {
        self.versions.values().map(|v| v.len()).sum()
    }

    pub fn bytes_purged(&self) -> u64 {
        self.bytes_purged
    }

    pub fn versions_purged(&self) -> u64 {
        self.versions_purged
    }

    pub fn reset_stats(&mut self) {
        self.bytes_purged = 0;
        self.versions_purged = 0;
    }
}

// ── Isolation Level ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl std::fmt::Display for IsolationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadUncommitted => write!(f, "READ UNCOMMITTED"),
            Self::ReadCommitted => write!(f, "READ COMMITTED"),
            Self::RepeatableRead => write!(f, "REPEATABLE READ"),
            Self::Serializable => write!(f, "SERIALIZABLE"),
        }
    }
}

/// Verification helper for checking isolation-level anomalies.
pub struct IsolationVerifier {
    level: IsolationLevel,
    read_set: HashSet<(String, u64)>,   // (table, row_id)
    write_set: HashSet<(String, u64)>,
    phantom_ranges: Vec<(String, String)>, // (table, predicate repr)
}

impl IsolationVerifier {
    pub fn new(level: IsolationLevel) -> Self {
        Self {
            level,
            read_set: HashSet::new(),
            write_set: HashSet::new(),
            phantom_ranges: Vec::new(),
        }
    }

    pub fn level(&self) -> IsolationLevel {
        self.level
    }

    /// Record a read operation.
    pub fn record_read(&mut self, table: &str, row_id: u64) {
        self.read_set.insert((table.to_string(), row_id));
    }

    /// Record a write operation.
    pub fn record_write(&mut self, table: &str, row_id: u64) {
        self.write_set.insert((table.to_string(), row_id));
    }

    /// Record a range predicate for phantom-read detection.
    pub fn record_range_predicate(&mut self, table: &str, predicate: &str) {
        self.phantom_ranges.push((table.to_string(), predicate.to_string()));
    }

    /// Check if the current transaction has a write-write conflict with another's write set.
    pub fn has_write_conflict(&self, other_writes: &HashSet<(String, u64)>) -> bool {
        !self.write_set.is_disjoint(other_writes)
    }

    /// Check if a non-repeatable read could occur.
    ///
    /// True if level is ReadUncommitted or ReadCommitted and there are reads that
    /// intersect with another transactions writes.
    pub fn can_have_non_repeatable_read(
        &self,
        other_writes: &HashSet<(String, u64)>,
    ) -> bool {
        match self.level {
            IsolationLevel::ReadUncommitted | IsolationLevel::ReadCommitted => {
                !self.read_set.is_disjoint(other_writes)
            }
            _ => false,
        }
    }

    /// Check if phantom reads are possible at this isolation level.
    pub fn can_have_phantom_read(&self) -> bool {
        match self.level {
            IsolationLevel::Serializable => false,
            _ => !self.phantom_ranges.is_empty(),
        }
    }

    pub fn read_set_size(&self) -> usize {
        self.read_set.len()
    }

    pub fn write_set_size(&self) -> usize {
        self.write_set.len()
    }
}

// ── Foreign Key Cascade ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CascadeAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl std::fmt::Display for CascadeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAction => write!(f, "NO ACTION"),
            Self::Restrict => write!(f, "RESTRICT"),
            Self::Cascade => write!(f, "CASCADE"),
            Self::SetNull => write!(f, "SET NULL"),
            Self::SetDefault => write!(f, "SET DEFAULT"),
        }
    }
}

/// A foreign-key constraint definition.
#[derive(Debug, Clone)]
pub struct ForeignKeyDef {
    pub name: String,
    pub child_table: String,
    pub child_columns: Vec<String>,
    pub parent_table: String,
    pub parent_columns: Vec<String>,
    pub on_delete: CascadeAction,
    pub on_update: CascadeAction,
}

/// Determines cascade operations that must happen when a parent row is modified.
#[derive(Debug, Clone)]
pub struct CascadeOp {
    pub child_table: String,
    pub child_columns: Vec<String>,
    pub action: CascadeAction,
    pub parent_key_values: Vec<String>, // stringified key values
}

/// Tracks foreign-key relationships and generates cascade operations.
pub struct ForeignKeyCascade {
    /// parent_table → list of FK definitions referencing it.
    fks_by_parent: HashMap<String, Vec<ForeignKeyDef>>,
}

impl ForeignKeyCascade {
    pub fn new() -> Self {
        Self {
            fks_by_parent: HashMap::new(),
        }
    }

    /// Register a foreign-key constraint.
    pub fn add_fk(&mut self, fk: ForeignKeyDef) {
        self.fks_by_parent
            .entry(fk.parent_table.clone())
            .or_default()
            .push(fk);
    }

    /// Remove all FKs whose child_table matches.
    pub fn remove_fks_for_child(&mut self, child_table: &str) {
        for fks in self.fks_by_parent.values_mut() {
            fks.retain(|fk| fk.child_table != child_table);
        }
    }

    /// Get the cascade operations triggered by deleting a row in `parent_table`.
    pub fn on_delete(
        &self,
        parent_table: &str,
        key_values: &[String],
    ) -> Vec<CascadeOp> {
        let Some(fks) = self.fks_by_parent.get(parent_table) else {
            return Vec::new();
        };
        fks.iter()
            .filter(|fk| fk.on_delete != CascadeAction::NoAction)
            .map(|fk| CascadeOp {
                child_table: fk.child_table.clone(),
                child_columns: fk.child_columns.clone(),
                action: fk.on_delete.clone(),
                parent_key_values: key_values.to_vec(),
            })
            .collect()
    }

    /// Get the cascade operations triggered by updating key columns in `parent_table`.
    pub fn on_update(
        &self,
        parent_table: &str,
        key_values: &[String],
    ) -> Vec<CascadeOp> {
        let Some(fks) = self.fks_by_parent.get(parent_table) else {
            return Vec::new();
        };
        fks.iter()
            .filter(|fk| fk.on_update != CascadeAction::NoAction)
            .map(|fk| CascadeOp {
                child_table: fk.child_table.clone(),
                child_columns: fk.child_columns.clone(),
                action: fk.on_update.clone(),
                parent_key_values: key_values.to_vec(),
            })
            .collect()
    }

    /// Check if deleting from `parent_table` would be blocked (RESTRICT).
    pub fn would_restrict_delete(&self, parent_table: &str) -> bool {
        self.fks_by_parent
            .get(parent_table)
            .map(|fks| fks.iter().any(|fk| fk.on_delete == CascadeAction::Restrict))
            .unwrap_or(false)
    }

    /// Number of FK definitions tracked.
    pub fn fk_count(&self) -> usize {
        self.fks_by_parent.values().map(|v| v.len()).sum()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_add_and_purge() {
        let mut gc = MvccGarbageCollector::new(0);
        gc.add_version(VersionedRow::new(1, 1, vec![1, 2, 3]));
        gc.add_version(VersionedRow::new(1, 5, vec![4, 5, 6]));
        gc.add_version(VersionedRow::new(1, 10, vec![7, 8, 9]));
        assert_eq!(gc.version_count(), 3);

        gc.advance_watermark(6);
        let purged = gc.purge();
        assert_eq!(purged, 1); // version txn_id=1 purged, txn_id=5 kept as latest ≤ watermark
        assert_eq!(gc.version_count(), 2);
    }

    #[test]
    fn gc_purge_tombstones() {
        let mut gc = MvccGarbageCollector::new(10);
        gc.add_version(VersionedRow::new_deleted(1, 5));
        gc.add_version(VersionedRow::new(2, 3, vec![1]));
        assert_eq!(gc.row_count(), 2);

        let purged = gc.purge_tombstones();
        assert_eq!(purged, 1); // row 1 tombstone removed
        assert_eq!(gc.row_count(), 1);
    }

    #[test]
    fn gc_watermark_only_advances_forward() {
        let mut gc = MvccGarbageCollector::new(10);
        gc.advance_watermark(5);
        assert_eq!(gc.watermark(), 10);
        gc.advance_watermark(20);
        assert_eq!(gc.watermark(), 20);
    }

    #[test]
    fn gc_stats() {
        let mut gc = MvccGarbageCollector::new(0);
        gc.add_version(VersionedRow::new(1, 1, vec![0; 100]));
        gc.add_version(VersionedRow::new(1, 5, vec![0; 50]));
        gc.advance_watermark(10);
        gc.purge();
        assert_eq!(gc.bytes_purged(), 100);
        assert_eq!(gc.versions_purged(), 1);
        gc.reset_stats();
        assert_eq!(gc.bytes_purged(), 0);
    }

    #[test]
    fn isolation_level_display() {
        assert_eq!(format!("{}", IsolationLevel::ReadCommitted), "READ COMMITTED");
        assert_eq!(format!("{}", IsolationLevel::Serializable), "SERIALIZABLE");
    }

    #[test]
    fn isolation_verifier_rw_conflict() {
        let mut v = IsolationVerifier::new(IsolationLevel::ReadCommitted);
        v.record_read("t1", 1);
        v.record_read("t1", 2);
        v.record_write("t1", 3);

        let mut other = HashSet::new();
        other.insert(("t1".to_string(), 2u64));
        assert!(v.can_have_non_repeatable_read(&other));
        assert!(!v.has_write_conflict(&other));

        other.insert(("t1".to_string(), 3u64));
        assert!(v.has_write_conflict(&other));
    }

    #[test]
    fn isolation_repeatable_read_blocks_non_repeatable() {
        let v = IsolationVerifier::new(IsolationLevel::RepeatableRead);
        let mut other = HashSet::new();
        other.insert(("t1".to_string(), 1u64));
        assert!(!v.can_have_non_repeatable_read(&other));
    }

    #[test]
    fn isolation_phantom_reads() {
        let mut v = IsolationVerifier::new(IsolationLevel::ReadCommitted);
        v.record_range_predicate("t1", "age > 25");
        assert!(v.can_have_phantom_read());

        let mut s = IsolationVerifier::new(IsolationLevel::Serializable);
        s.record_range_predicate("t1", "age > 25");
        assert!(!s.can_have_phantom_read());
    }

    #[test]
    fn fk_cascade_on_delete() {
        let mut fkc = ForeignKeyCascade::new();
        fkc.add_fk(ForeignKeyDef {
            name: "fk_order_user".into(),
            child_table: "orders".into(),
            child_columns: vec!["user_id".into()],
            parent_table: "users".into(),
            parent_columns: vec!["id".into()],
            on_delete: CascadeAction::Cascade,
            on_update: CascadeAction::NoAction,
        });
        let ops = fkc.on_delete("users", &["42".into()]);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].child_table, "orders");
        assert_eq!(ops[0].action, CascadeAction::Cascade);
    }

    #[test]
    fn fk_restrict_prevents_delete() {
        let mut fkc = ForeignKeyCascade::new();
        fkc.add_fk(ForeignKeyDef {
            name: "fk_restrict".into(),
            child_table: "orders".into(),
            child_columns: vec!["user_id".into()],
            parent_table: "users".into(),
            parent_columns: vec!["id".into()],
            on_delete: CascadeAction::Restrict,
            on_update: CascadeAction::NoAction,
        });
        assert!(fkc.would_restrict_delete("users"));
        assert!(!fkc.would_restrict_delete("products"));
    }

    #[test]
    fn fk_cascade_on_update() {
        let mut fkc = ForeignKeyCascade::new();
        fkc.add_fk(ForeignKeyDef {
            name: "fk_cascade_update".into(),
            child_table: "orders".into(),
            child_columns: vec!["user_id".into()],
            parent_table: "users".into(),
            parent_columns: vec!["id".into()],
            on_delete: CascadeAction::NoAction,
            on_update: CascadeAction::SetNull,
        });
        let ops = fkc.on_update("users", &["1".into()]);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].action, CascadeAction::SetNull);
    }

    #[test]
    fn fk_remove_child() {
        let mut fkc = ForeignKeyCascade::new();
        fkc.add_fk(ForeignKeyDef {
            name: "fk1".into(),
            child_table: "orders".into(),
            child_columns: vec!["uid".into()],
            parent_table: "users".into(),
            parent_columns: vec!["id".into()],
            on_delete: CascadeAction::Cascade,
            on_update: CascadeAction::NoAction,
        });
        assert_eq!(fkc.fk_count(), 1);
        fkc.remove_fks_for_child("orders");
        assert_eq!(fkc.fk_count(), 0);
    }
}
