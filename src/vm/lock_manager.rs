// C3: Cross-Connection Deadlock Detection — Global Lock Manager
//
// A single global `GLOBAL_LOCK_TABLE` (Arc<Mutex<LockTable>>) is shared by
// all VM instances. Each table can be locked in Shared or Exclusive mode by
// one transaction at a time. The wait-for graph tracks which txn is waiting
// for which tables, and DFS is used to detect cycles (deadlocks).

use crate::error::{KkdbError, Result};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Lock granularity mode for a table.
#[derive(Debug, Clone, PartialEq)]
pub enum LockMode {
    /// Multiple readers can hold Shared locks simultaneously.
    Shared,
    /// Only one writer can hold an Exclusive lock; blocks all others.
    Exclusive,
}

/// A single lock held on a named table.
#[derive(Debug, Clone)]
pub struct LockEntry {
    pub mode: LockMode,
    pub holder_txn: u64,
}

/// The global table-level lock manager.
#[derive(Debug, Default)]
pub struct LockTable {
    /// table_name (lowercase) → currently held lock
    pub locks: HashMap<String, LockEntry>,
    /// txn_id → list of table names it is WAITING to acquire
    pub waiters: HashMap<u64, Vec<String>>,
}

impl LockTable {
    pub fn new() -> Self {
        Self {
            locks: HashMap::new(),
            waiters: HashMap::new(),
        }
    }

    /// Try to acquire `mode` lock on `table` for `txn_id`.
    ///
    /// Rules:
    /// - Same txn can upgrade Shared→Exclusive or re-enter any mode: OK.
    /// - Two different txns with Shared locks: OK.
    /// - Any Exclusive conflict with a different txn: check for cycle, then err.
    pub fn try_acquire(&mut self, table: &str, mode: LockMode, txn_id: u64) -> Result<()> {
        let tbl = table.to_ascii_lowercase();

        if let Some(entry) = self.locks.get(&tbl) {
            if entry.holder_txn == txn_id {
                // Same txn: upgrade or re-enter — always OK
                if mode == LockMode::Exclusive {
                    self.locks.insert(tbl, LockEntry { mode, holder_txn: txn_id });
                }
                return Ok(());
            }
            // Different txn holds a lock on this table
            let conflict = match (&entry.mode, &mode) {
                (LockMode::Shared, LockMode::Shared) => false, // shared-shared: OK
                _ => true,
            };
            if conflict {
                // Register as waiter so cycle detection can see us
                self.waiters.entry(txn_id).or_default().push(tbl.clone());
                // Check for deadlock cycle
                let has_deadlock = self.has_cycle(txn_id);
                // Clean up waiter registration (we won't block; we return error immediately)
                if let Some(v) = self.waiters.get_mut(&txn_id) {
                    v.retain(|t| t != &tbl);
                    if v.is_empty() {
                        self.waiters.remove(&txn_id);
                    }
                }
                if has_deadlock {
                    return Err(KkdbError::Internal(format!(
                        "Deadlock detected: txn {} and txn {} form a cycle on table `{}`",
                        txn_id, entry.holder_txn, tbl
                    )));
                }
                // No cycle detected but lock is held by another txn — report conflict
                return Err(KkdbError::Internal(format!(
                    "Lock conflict: table `{}` is {:?}-locked by txn {}, txn {} cannot acquire {:?}",
                    tbl, entry.mode, entry.holder_txn, txn_id, mode
                )));
            }
            // Shared-Shared: fall through to grant
        }

        self.locks.insert(tbl, LockEntry { mode, holder_txn: txn_id });
        Ok(())
    }

    /// Release all locks held by `txn_id`.
    pub fn release_all(&mut self, txn_id: u64) {
        self.locks.retain(|_, entry| entry.holder_txn != txn_id);
        self.waiters.remove(&txn_id);
    }

    /// DFS cycle detection in the wait-for graph.
    ///
    /// `start` is the txn we are checking for a cycle from.
    /// We follow: start → holds table X → which txn holds X → that txn waits for Y → ...
    fn has_cycle(&self, start: u64) -> bool {
        let mut visited = HashSet::new();
        let mut stack = vec![start];

        while let Some(txn) = stack.pop() {
            if !visited.insert(txn) {
                return true; // Cycle found
            }
            // Find which tables this txn is waiting for
            if let Some(waiting_tables) = self.waiters.get(&txn) {
                for tbl in waiting_tables {
                    // Find who holds the lock on this table
                    if let Some(entry) = self.locks.get(tbl) {
                        if entry.holder_txn != txn {
                            stack.push(entry.holder_txn);
                        }
                    }
                }
            }
        }
        false
    }
}

// ── Global singleton ──────────────────────────────────────────────────────

use std::sync::OnceLock;

static GLOBAL_LOCK_TABLE_INNER: OnceLock<Arc<Mutex<LockTable>>> = OnceLock::new();

/// Returns the process-wide shared lock table.
pub fn global_lock_table() -> Arc<Mutex<LockTable>> {
    GLOBAL_LOCK_TABLE_INNER
        .get_or_init(|| Arc::new(Mutex::new(LockTable::new())))
        .clone()
}
