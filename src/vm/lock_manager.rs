// C3: Cross-Connection Deadlock Detection — Global Lock Manager
//
// A single global `GLOBAL_LOCK_TABLE` (Arc<Mutex<LockTable>>) is shared by
// all VM instances. Each table can be locked in Shared or Exclusive mode.
// Multiple Shared locks from different txns may coexist; Exclusive locks
// block all other txns.

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
///
/// I5 fix: `locks` is now `HashMap<table, Vec<LockEntry>>` to support
/// multiple concurrent Shared holders without overwriting each other.
#[derive(Debug, Default)]
pub struct LockTable {
    /// table_name (lowercase) → list of currently held locks
    pub locks: HashMap<String, Vec<LockEntry>>,
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
    /// - Multiple different txns with Shared locks: OK.
    /// - Any Exclusive request from a different txn: conflict if ANY other lock is held.
    pub fn try_acquire(&mut self, table: &str, mode: LockMode, txn_id: u64) -> Result<()> {
        let tbl = table.to_ascii_lowercase();

        // Use a scoped block so the `&mut` borrow of `self.locks` ends before
        // we touch `self.waiters` below.
        let conflict_info: Option<(u64, LockMode)> = {
            let entries = self.locks.entry(tbl.clone()).or_insert_with(Vec::new);

            // Check if this txn already holds a lock on this table
            if let Some(mine) = entries.iter_mut().find(|e| e.holder_txn == txn_id) {
                // Upgrade to Exclusive if needed; otherwise re-enter is a no-op
                if mode == LockMode::Exclusive {
                    mine.mode = LockMode::Exclusive;
                }
                return Ok(());
            }

            // Check for conflicts with OTHER txns — collect conflict info first before
            // touching self.waiters (to avoid simultaneous borrow of self.locks).
            entries.iter()
                .filter(|e| e.holder_txn != txn_id)
                .find(|e| !matches!((&e.mode, &mode), (LockMode::Shared, LockMode::Shared)))
                .map(|e| (e.holder_txn, e.mode.clone()))
            // `entries` borrow is released here at end of block
        };

        if let Some((holder_txn, holder_mode)) = conflict_info {
            // Register as waiter so cycle detection can see us
            self.waiters.entry(txn_id).or_default().push(tbl.clone());
            let has_deadlock = self.has_cycle(txn_id);
            // Clean up waiter registration
            if let Some(v) = self.waiters.get_mut(&txn_id) {
                v.retain(|t| t != &tbl);
                if v.is_empty() {
                    self.waiters.remove(&txn_id);
                }
            }
            if has_deadlock {
                return Err(KkdbError::Internal(format!(
                    "Deadlock detected: txn {} and txn {} form a cycle on table `{}`",
                    txn_id, holder_txn, tbl
                )));
            }
            return Err(KkdbError::Internal(format!(
                "Lock conflict: table `{}` is {:?}-locked by txn {}, txn {} cannot acquire {:?}",
                tbl, holder_mode, holder_txn, txn_id, mode
            )));
        }

        // No conflict — grant the lock
        self.locks
            .entry(tbl)
            .or_insert_with(Vec::new)
            .push(LockEntry { mode, holder_txn: txn_id });
        Ok(())
    }

    /// Release all locks held by `txn_id`.
    pub fn release_all(&mut self, txn_id: u64) {
        for entries in self.locks.values_mut() {
            entries.retain(|e| e.holder_txn != txn_id);
        }
        // Remove now-empty table entries to keep the map compact
        self.locks.retain(|_, entries| !entries.is_empty());
        self.waiters.remove(&txn_id);
    }

    /// Wait-for graph cycle detection.
    ///
    /// Returns `true` iff there is a cycle in the wait-for graph that includes `start`.
    ///
    /// S4 fix: The previous DFS implementation returned `true` whenever it visited any
    /// already-seen node, which produced false positives when multiple transactions waited
    /// on the same holder (diamond-shaped waits). The correct algorithm is:
    ///
    /// 1. Build a "waiting_for" map: txn → set of txns it is directly blocked by.
    /// 2. Walk from `start` following the waiting_for edges.
    /// 3. Declare a deadlock only when we reach `start` again (true cycle).
    fn has_cycle(&self, start: u64) -> bool {
        // Build a direct "waiting_for" adjacency: for each waiter, collect the txns
        // that currently hold locks on the tables it is waiting for.
        // We do not need a full graph — we only care about cycles reachable from `start`.
        let mut visited: HashSet<u64> = HashSet::new();
        let mut stack: Vec<u64> = vec![start];

        while let Some(txn) = stack.pop() {
            // Collect the txns that `txn` is directly waiting for
            if let Some(waiting_tables) = self.waiters.get(&txn) {
                for tbl in waiting_tables {
                    if let Some(entries) = self.locks.get(tbl) {
                        for entry in entries {
                            let holder = entry.holder_txn;
                            if holder == txn {
                                continue; // skip self-locks
                            }
                            if holder == start {
                                // We found a path back to `start` — genuine cycle.
                                return true;
                            }
                            if visited.insert(holder) {
                                // First time seeing this holder — explore its wait chain
                                stack.push(holder);
                            }
                            // If already visited (but != start), it is part of a different
                            // cycle not involving `start`, so we do not report it.
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
