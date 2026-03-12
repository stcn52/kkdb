// C1: MVCC Undo Log — PostgreSQL-style multi-version concurrency control
//
// Each DML operation within an explicit transaction appends an UndoEntry.
// On ROLLBACK, entries are replayed in reverse order to undo changes.
// On COMMIT, the log is simply cleared (COW pager handles physical rollback).
//
// ## Enhancements over basic undo log
//
// 1. **Transaction version stamping** — each undo entry carries `txn_id` (xmin)
//    so that visibility checks can determine which transaction created/deleted a row.
// 2. **Savepoint markers** — `Savepoint` entries in the undo log allow partial
//    rollback to a named savepoint by replaying only entries after the marker.
// 3. **Active transaction registry** — `TransactionRegistry` tracks active txn IDs
//    and provides snapshot isolation via `MvccSnapshot`.
// 4. **Garbage collection** — `UndoLog::purge(min_active_txn_id)` removes entries
//    that are no longer needed for any active reader.
// 5. **Undo log statistics** — `UndoLog::stats()` reports size, entry count, etc.

use crate::types::Row;

/// One undo-able operation recorded before or after a DML statement.
#[derive(Debug, Clone)]
pub enum UndoEntry {
    /// INSERT was performed — undo by deleting rowid from the table
    Insert {
        table: String,
        rowid: i64,
        txn_id: u64,
    },
    /// UPDATE was performed — undo by writing the old row back
    Update {
        table: String,
        rowid: i64,
        old_row: Row,
        txn_id: u64,
    },
    /// DELETE was performed — undo by re-inserting the row
    Delete {
        table: String,
        rowid: i64,
        old_row: Row,
        txn_id: u64,
    },
    /// Savepoint marker — ROLLBACK TO replays entries after this marker
    Savepoint {
        name: String,
        txn_id: u64,
    },
}

impl UndoEntry {
    /// Returns the transaction ID that created this entry.
    pub fn txn_id(&self) -> u64 {
        match self {
            UndoEntry::Insert { txn_id, .. } => *txn_id,
            UndoEntry::Update { txn_id, .. } => *txn_id,
            UndoEntry::Delete { txn_id, .. } => *txn_id,
            UndoEntry::Savepoint { txn_id, .. } => *txn_id,
        }
    }

    /// Returns the table name affected, if any.
    pub fn table_name(&self) -> Option<&str> {
        match self {
            UndoEntry::Insert { table, .. } => Some(table),
            UndoEntry::Update { table, .. } => Some(table),
            UndoEntry::Delete { table, .. } => Some(table),
            UndoEntry::Savepoint { .. } => None,
        }
    }
}

// ── Managed Undo Log ────────────────────────────────────────────────────────

/// A managed undo log that supports savepoints, partial rollback, and statistics.
#[derive(Debug, Clone, Default)]
pub struct UndoLog {
    entries: Vec<UndoEntry>,
    /// Byte-size estimate for statistics (approximate memory usage).
    size_bytes: usize,
}

impl UndoLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            size_bytes: 0,
        }
    }

    /// Push an undo entry and update size estimate.
    pub fn push(&mut self, entry: UndoEntry) {
        self.size_bytes += Self::entry_size(&entry);
        self.entries.push(entry);
    }

    /// Record a savepoint marker in the undo log.
    pub fn savepoint(&mut self, name: &str, txn_id: u64) {
        self.push(UndoEntry::Savepoint {
            name: name.to_string(),
            txn_id,
        });
    }

    /// Number of entries in the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries (on COMMIT).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.size_bytes = 0;
    }

    /// Return entries added after the named savepoint, in reverse order.
    /// Used for ROLLBACK TO <savepoint> to replay undo operations.
    /// The savepoint marker itself is NOT included in the returned entries.
    /// Entries after the savepoint are removed from the log.
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Vec<UndoEntry> {
        // Find the latest savepoint with this name
        let pos = self.entries.iter().rposition(|e| {
            matches!(e, UndoEntry::Savepoint { name: n, .. } if n.eq_ignore_ascii_case(name))
        });
        match pos {
            Some(idx) => {
                // Drain entries *after* the savepoint marker (reverse order for undo)
                let mut undone: Vec<UndoEntry> = self.entries.drain(idx + 1..).collect();
                undone.reverse();
                // Recalculate size
                self.size_bytes = self.entries.iter().map(|e| Self::entry_size(e)).sum();
                undone
            }
            None => Vec::new(), // Savepoint not found — no entries to undo
        }
    }

    /// Purge entries that are no longer needed for MVCC visibility.
    /// Removes entries with `txn_id < min_active_txn_id` — these transactions
    /// are guaranteed to be committed and visible to all active readers.
    pub fn purge(&mut self, min_active_txn_id: u64) {
        self.entries.retain(|e| e.txn_id() >= min_active_txn_id);
        self.size_bytes = self.entries.iter().map(|e| Self::entry_size(e)).sum();
    }

    /// Iterate over all entries (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &UndoEntry> {
        self.entries.iter()
    }

    /// Iterate in reverse (newest first), for rollback replay.
    pub fn iter_rev(&self) -> impl Iterator<Item = &UndoEntry> {
        self.entries.iter().rev()
    }

    /// Return statistics about the undo log.
    pub fn stats(&self) -> UndoLogStats {
        let mut inserts = 0u64;
        let mut updates = 0u64;
        let mut deletes = 0u64;
        let mut savepoints = 0u64;
        for e in &self.entries {
            match e {
                UndoEntry::Insert { .. } => inserts += 1,
                UndoEntry::Update { .. } => updates += 1,
                UndoEntry::Delete { .. } => deletes += 1,
                UndoEntry::Savepoint { .. } => savepoints += 1,
            }
        }
        UndoLogStats {
            total_entries: self.entries.len() as u64,
            size_bytes: self.size_bytes as u64,
            inserts,
            updates,
            deletes,
            savepoints,
        }
    }

    /// Approximate memory size of a single entry.
    fn entry_size(entry: &UndoEntry) -> usize {
        // Base size for enum discriminant + fixed fields
        let base = 64; // conservative estimate for enum + String + i64 + u64
        match entry {
            UndoEntry::Insert { table, .. } => base + table.len(),
            UndoEntry::Update {
                table, old_row, ..
            } => {
                base + table.len()
                    + old_row
                        .iter()
                        .map(|v| std::mem::size_of_val(v) + 16)
                        .sum::<usize>()
            }
            UndoEntry::Delete {
                table, old_row, ..
            } => {
                base + table.len()
                    + old_row
                        .iter()
                        .map(|v| std::mem::size_of_val(v) + 16)
                        .sum::<usize>()
            }
            UndoEntry::Savepoint { name, .. } => base + name.len(),
        }
    }
}

/// Statistics about the undo log.
#[derive(Debug, Clone)]
pub struct UndoLogStats {
    pub total_entries: u64,
    pub size_bytes: u64,
    pub inserts: u64,
    pub updates: u64,
    pub deletes: u64,
    pub savepoints: u64,
}

// ── MVCC Isolation Levels ───────────────────────────────────────────────────

/// Transaction isolation level.
///
/// - `Serializable` (default): snapshot taken once at BEGIN; reads never see
///   later commits (true snapshot isolation).
/// - `ReadCommitted`: snapshot refreshed before every SQL statement within the
///   transaction, so each statement sees the latest committed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    Serializable,
    ReadCommitted,
}

impl Default for IsolationLevel {
    fn default() -> Self {
        Self::Serializable
    }
}

// ── MVCC Snapshot & Transaction Registry ────────────────────────────────────

/// A point-in-time MVCC read snapshot.
///
/// Created at BEGIN time. Determines row visibility:
/// - A row created by `txn_id <= snapshot_txn_id` AND `txn_id` was committed
///   at snapshot time → visible.
/// - A row created by `txn_id > snapshot_txn_id` → invisible (created after
///   our snapshot).
/// - A row created by a transaction that was active (uncommitted) at snapshot
///   time → invisible.
#[derive(Debug, Clone)]
pub struct MvccSnapshot {
    /// The transaction ID of the reader who created this snapshot.
    pub reader_txn_id: u64,
    /// The set of transaction IDs that were active (uncommitted) when this
    /// snapshot was created. Rows from these transactions are invisible.
    pub active_txn_ids: Vec<u64>,
    /// The highest committed transaction ID at snapshot creation time.
    /// Rows from `txn_id <= max_committed` (and not in `active_txn_ids`) are visible.
    pub max_committed_txn_id: u64,
}

impl MvccSnapshot {
    /// Check if a row version created by `creator_txn_id` is visible.
    pub fn is_visible(&self, creator_txn_id: u64) -> bool {
        // Our own writes are always visible
        if creator_txn_id == self.reader_txn_id {
            return true;
        }
        // Created after our snapshot → invisible
        if creator_txn_id > self.max_committed_txn_id {
            return false;
        }
        // Was active (uncommitted) at snapshot time → invisible
        if self.active_txn_ids.contains(&creator_txn_id) {
            return false;
        }
        // Committed before our snapshot → visible
        true
    }
}

/// Global transaction registry for MVCC.
///
/// Tracks active transactions and provides snapshot creation.
/// In a single-connection embedded database, this is relatively simple,
/// but the structure supports future multi-connection use.
#[derive(Debug, Clone)]
pub struct TransactionRegistry {
    /// Set of currently active (uncommitted) transaction IDs.
    active_txns: Vec<u64>,
    /// Monotonically increasing transaction ID counter.
    next_txn_id: u64,
    /// Highest committed transaction ID.
    max_committed: u64,
}

impl Default for TransactionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TransactionRegistry {
    pub fn new() -> Self {
        Self {
            active_txns: Vec::new(),
            next_txn_id: 1,
            max_committed: 0,
        }
    }

    /// Begin a new transaction, returning its unique ID.
    pub fn begin(&mut self) -> u64 {
        let txn_id = self.next_txn_id;
        self.next_txn_id += 1;
        self.active_txns.push(txn_id);
        txn_id
    }

    /// Mark a transaction as committed.
    pub fn commit(&mut self, txn_id: u64) {
        self.active_txns.retain(|&id| id != txn_id);
        if txn_id > self.max_committed {
            self.max_committed = txn_id;
        }
    }

    /// Mark a transaction as aborted (rolled back).
    pub fn abort(&mut self, txn_id: u64) {
        self.active_txns.retain(|&id| id != txn_id);
    }

    /// Create a snapshot for the given reader transaction.
    pub fn snapshot(&self, reader_txn_id: u64) -> MvccSnapshot {
        MvccSnapshot {
            reader_txn_id,
            active_txn_ids: self.active_txns.clone(),
            max_committed_txn_id: self.max_committed,
        }
    }

    /// Return the minimum active transaction ID, or `next_txn_id` if none active.
    /// Used for undo log garbage collection.
    pub fn min_active_txn_id(&self) -> u64 {
        self.active_txns.iter().copied().min().unwrap_or(self.next_txn_id)
    }

    /// Number of currently active transactions.
    pub fn active_count(&self) -> usize {
        self.active_txns.len()
    }

    /// The next transaction ID that will be assigned.
    pub fn next_id(&self) -> u64 {
        self.next_txn_id
    }

    /// All currently active transaction IDs.
    pub fn active_txn_ids(&self) -> &[u64] {
        &self.active_txns
    }
}

// ── MVCC Visibility Filter ─────────────────────────────────────────────────

/// Determine which row IDs should be hidden or restored based on the undo log
/// and an MVCC snapshot. This is used by SELECT to apply snapshot isolation.
///
/// Returns `(invisible_rowids, restored_rows)`:
/// - `invisible_rowids`: set of rowids that were inserted by transactions
///   not visible in the snapshot — these rows should be excluded from results.
/// - `restored_rows`: rows that were deleted by transactions not visible in
///   the snapshot — these rows should be added back to results.
pub fn compute_visibility_delta(
    undo_log: &UndoLog,
    snapshot: &MvccSnapshot,
    table_name: &str,
) -> (std::collections::HashSet<i64>, Vec<(i64, Row)>) {
    let mut invisible_rowids = std::collections::HashSet::new();
    let mut restored_rows = Vec::new();
    let table_lower = table_name.to_ascii_lowercase();

    for entry in undo_log.iter() {
        match entry {
            UndoEntry::Insert {
                table,
                rowid,
                txn_id,
            } => {
                // If the INSERT was done by a transaction not visible in our snapshot,
                // the inserted row should be invisible.
                if table.to_ascii_lowercase() == table_lower
                    && !snapshot.is_visible(*txn_id)
                {
                    invisible_rowids.insert(*rowid);
                }
            }
            UndoEntry::Delete {
                table,
                rowid,
                old_row,
                txn_id,
            } => {
                // If the DELETE was done by a transaction not visible in our snapshot,
                // the deleted row should still be visible (restore it).
                if table.to_ascii_lowercase() == table_lower
                    && !snapshot.is_visible(*txn_id)
                {
                    restored_rows.push((*rowid, old_row.clone()));
                }
            }
            UndoEntry::Update {
                table,
                rowid,
                old_row,
                txn_id,
            } => {
                // If the UPDATE was done by an invisible transaction,
                // we should see the old version of the row.
                // Mark the current version as invisible and restore the old version.
                if table.to_ascii_lowercase() == table_lower
                    && !snapshot.is_visible(*txn_id)
                {
                    invisible_rowids.insert(*rowid);
                    restored_rows.push((*rowid, old_row.clone()));
                }
            }
            UndoEntry::Savepoint { .. } => {}
        }
    }

    (invisible_rowids, restored_rows)
}

// ── Tests ───────────────────────────────────────────────────────────────────

// ── Row-Level Lock Manager ──────────────────────────────────────────────────

/// Row-level lock key: (table_lowercase, rowid).
pub type RowLockKey = (String, i64);

/// Row-level lock manager for MVCC write-write conflict detection.
///
/// When a transaction performs UPDATE or DELETE on a row, it acquires an
/// exclusive lock on (table, rowid). If another active transaction already
/// holds a lock on the same row, a write-write conflict is raised.
///
/// ## Optimistic Concurrency Control
///
/// In addition to row locks, each transaction records its **read set** —
/// the set of (table, rowid) pairs it has seen via SELECT or SELECT FOR UPDATE.
/// At COMMIT time, the read set can be validated: if any row in the read set
/// was modified by a committed transaction since our snapshot, the transaction
/// must abort (first-committer-wins).
#[derive(Debug, Clone, Default)]
pub struct RowLockManager {
    /// (table_lowercase, rowid) → holder_txn_id
    pub locks: std::collections::HashMap<RowLockKey, u64>,
    /// txn_id → set of row keys it has locked
    pub txn_locks: std::collections::HashMap<u64, Vec<RowLockKey>>,
    /// txn_id → read set for optimistic validation
    pub read_sets: std::collections::HashMap<u64, Vec<RowLockKey>>,
    /// (table_lowercase, rowid) → last committed txn_id that modified this row
    pub committed_versions: std::collections::HashMap<RowLockKey, u64>,
}

impl RowLockManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire an exclusive row lock for `txn_id`.
    ///
    /// Returns `Ok(())` if the lock is granted (either fresh or already held by same txn).
    /// Returns `Err` if another transaction holds the lock (write-write conflict).
    pub fn try_lock_row(
        &mut self,
        table: &str,
        rowid: i64,
        txn_id: u64,
    ) -> crate::error::Result<()> {
        let key = (table.to_ascii_lowercase(), rowid);

        if let Some(&holder) = self.locks.get(&key) {
            if holder == txn_id {
                return Ok(()); // already hold it
            }
            return Err(crate::error::KkdbError::Internal(format!(
                "Write-write conflict: row ({}, {}) is locked by txn {}, txn {} cannot acquire",
                key.0, key.1, holder, txn_id
            )));
        }

        self.locks.insert(key.clone(), txn_id);
        self.txn_locks.entry(txn_id).or_default().push(key);
        Ok(())
    }

    /// Record a row in the read set for optimistic validation.
    pub fn record_read(
        &mut self,
        table: &str,
        rowid: i64,
        txn_id: u64,
    ) {
        let key = (table.to_ascii_lowercase(), rowid);
        let set = self.read_sets.entry(txn_id).or_default();
        if !set.contains(&key) {
            set.push(key);
        }
    }

    /// Release all row locks held by `txn_id`.
    pub fn release_all(&mut self, txn_id: u64) {
        if let Some(keys) = self.txn_locks.remove(&txn_id) {
            for key in keys {
                self.locks.remove(&key);
            }
        }
        self.read_sets.remove(&txn_id);
    }

    /// Mark all rows modified by `txn_id` as committed at this version.
    /// Called at COMMIT time before releasing locks.
    pub fn commit_version(&mut self, txn_id: u64) {
        if let Some(keys) = self.txn_locks.get(&txn_id) {
            for key in keys {
                self.committed_versions.insert(key.clone(), txn_id);
            }
        }
    }

    /// Optimistic validation: check that no row in `txn_id`'s read set
    /// was modified by a committed transaction with ID > `snapshot_txn_id`.
    ///
    /// Returns `Ok(())` if validation passes (no conflicts).
    /// Returns `Err` if any row was modified after our snapshot (must abort).
    pub fn validate_read_set(
        &self,
        txn_id: u64,
        snapshot_txn_id: u64,
    ) -> crate::error::Result<()> {
        if let Some(reads) = self.read_sets.get(&txn_id) {
            for key in reads {
                if let Some(&committed_by) = self.committed_versions.get(key) {
                    if committed_by > snapshot_txn_id && committed_by != txn_id {
                        return Err(crate::error::KkdbError::Internal(format!(
                            "Serialization failure: row ({}, {}) was modified by committed txn {} after snapshot {}",
                            key.0, key.1, committed_by, snapshot_txn_id
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Number of row locks currently held.
    pub fn lock_count(&self) -> usize {
        self.locks.len()
    }

    /// Number of rows in the read set for a transaction.
    pub fn read_set_size(&self, txn_id: u64) -> usize {
        self.read_sets.get(&txn_id).map_or(0, |s| s.len())
    }

    /// Garbage-collect committed_versions for rows with version < min_txn_id.
    /// This prevents the committed_versions map from growing unboundedly.
    pub fn gc_versions(&mut self, min_txn_id: u64) {
        self.committed_versions.retain(|_, &mut v| v >= min_txn_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Value;

    #[test]
    fn test_undo_entry_txn_id() {
        let e = UndoEntry::Insert {
            table: "t".into(),
            rowid: 1,
            txn_id: 42,
        };
        assert_eq!(e.txn_id(), 42);
        assert_eq!(e.table_name(), Some("t"));

        let sp = UndoEntry::Savepoint {
            name: "sp1".into(),
            txn_id: 10,
        };
        assert_eq!(sp.txn_id(), 10);
        assert!(sp.table_name().is_none());
    }

    #[test]
    fn test_undo_log_push_and_stats() {
        let mut log = UndoLog::new();
        assert!(log.is_empty());

        log.push(UndoEntry::Insert {
            table: "users".into(),
            rowid: 1,
            txn_id: 1,
        });
        log.push(UndoEntry::Update {
            table: "users".into(),
            rowid: 1,
            old_row: vec![Value::Integer(1), Value::Text("old".into())],
            txn_id: 1,
        });
        log.push(UndoEntry::Delete {
            table: "users".into(),
            rowid: 2,
            old_row: vec![Value::Integer(2)],
            txn_id: 1,
        });
        assert_eq!(log.len(), 3);

        let stats = log.stats();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.inserts, 1);
        assert_eq!(stats.updates, 1);
        assert_eq!(stats.deletes, 1);
        assert!(stats.size_bytes > 0);
    }

    #[test]
    fn test_undo_log_savepoint_rollback() {
        let mut log = UndoLog::new();

        // Initial insert
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 1,
            txn_id: 1,
        });

        // Savepoint
        log.savepoint("sp1", 1);

        // Insert after savepoint
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 2,
            txn_id: 1,
        });
        log.push(UndoEntry::Update {
            table: "t".into(),
            rowid: 1,
            old_row: vec![Value::Integer(100)],
            txn_id: 1,
        });

        assert_eq!(log.len(), 4); // Insert + Savepoint + Insert + Update

        // Rollback to savepoint — should return the 2 entries after sp1 in reverse
        let undone = log.rollback_to_savepoint("sp1");
        assert_eq!(undone.len(), 2);
        // First entry in reverse = Update, second = Insert
        assert!(matches!(undone[0], UndoEntry::Update { rowid: 1, .. }));
        assert!(matches!(undone[1], UndoEntry::Insert { rowid: 2, .. }));

        // Log should still have the initial insert + savepoint marker
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn test_undo_log_purge() {
        let mut log = UndoLog::new();
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 1,
            txn_id: 1,
        });
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 2,
            txn_id: 5,
        });
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 3,
            txn_id: 10,
        });

        // Purge entries with txn_id < 5
        log.purge(5);
        assert_eq!(log.len(), 2);
        // Only txn_id 5 and 10 remain
        let ids: Vec<u64> = log.iter().map(|e| e.txn_id()).collect();
        assert_eq!(ids, vec![5, 10]);
    }

    #[test]
    fn test_mvcc_snapshot_visibility() {
        let snap = MvccSnapshot {
            reader_txn_id: 5,
            active_txn_ids: vec![3, 4],
            max_committed_txn_id: 6,
        };

        // Own writes visible
        assert!(snap.is_visible(5));
        // Committed before snapshot visible
        assert!(snap.is_visible(1));
        assert!(snap.is_visible(2));
        // Active at snapshot time → invisible
        assert!(!snap.is_visible(3));
        assert!(!snap.is_visible(4));
        // Committed but visible (txn 6 committed before snapshot)
        assert!(snap.is_visible(6));
        // Created after snapshot → invisible
        assert!(!snap.is_visible(7));
        assert!(!snap.is_visible(100));
    }

    #[test]
    fn test_transaction_registry_lifecycle() {
        let mut reg = TransactionRegistry::new();

        let t1 = reg.begin();
        assert_eq!(t1, 1);
        let t2 = reg.begin();
        assert_eq!(t2, 2);
        assert_eq!(reg.active_count(), 2);

        // Snapshot for t2 sees t1 as active
        let snap = reg.snapshot(t2);
        assert_eq!(snap.active_txn_ids, vec![1, 2]);
        assert_eq!(snap.max_committed_txn_id, 0);

        // Commit t1
        reg.commit(t1);
        assert_eq!(reg.active_count(), 1);

        // New snapshot sees t1 as committed
        let snap2 = reg.snapshot(t2);
        assert_eq!(snap2.active_txn_ids, vec![2]);
        assert_eq!(snap2.max_committed_txn_id, 1);
        assert!(snap2.is_visible(1)); // t1 committed
        assert!(snap2.is_visible(2)); // own writes

        // Abort t2
        reg.abort(t2);
        assert_eq!(reg.active_count(), 0);
        assert_eq!(reg.min_active_txn_id(), 3); // next_txn_id
    }

    #[test]
    fn test_transaction_registry_min_active() {
        let mut reg = TransactionRegistry::new();
        assert_eq!(reg.min_active_txn_id(), 1); // no active → returns next_txn_id

        let t1 = reg.begin();
        let _t2 = reg.begin();
        let _t3 = reg.begin();
        assert_eq!(reg.min_active_txn_id(), t1); // t1 is oldest active

        reg.commit(t1);
        assert_eq!(reg.min_active_txn_id(), 2); // t2 is now oldest
    }

    #[test]
    fn test_undo_log_clear_on_commit() {
        let mut log = UndoLog::new();
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 1,
            txn_id: 1,
        });
        log.savepoint("sp", 1);
        log.push(UndoEntry::Delete {
            table: "t".into(),
            rowid: 2,
            old_row: vec![],
            txn_id: 1,
        });
        assert_eq!(log.len(), 3);
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.stats().size_bytes, 0);
    }

    #[test]
    fn test_nested_savepoints() {
        let mut log = UndoLog::new();
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 1,
            txn_id: 1,
        });
        log.savepoint("sp_outer", 1);
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 2,
            txn_id: 1,
        });
        log.savepoint("sp_inner", 1);
        log.push(UndoEntry::Insert {
            table: "t".into(),
            rowid: 3,
            txn_id: 1,
        });

        // Rollback inner savepoint
        let undone = log.rollback_to_savepoint("sp_inner");
        assert_eq!(undone.len(), 1);
        assert!(matches!(undone[0], UndoEntry::Insert { rowid: 3, .. }));
        assert_eq!(log.len(), 4); // Insert(1) + SP(outer) + Insert(2) + SP(inner)

        // Rollback outer savepoint — undoes Insert(2) + SP(inner)
        let undone2 = log.rollback_to_savepoint("sp_outer");
        assert_eq!(undone2.len(), 2); // SP(inner) reversed first, then Insert(2)
        assert_eq!(log.len(), 2); // Insert(1) + SP(outer)
    }
}
