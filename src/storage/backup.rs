// R11 – Page-level checksum verification & incremental backup support.
//
// Provides:
//   - `PageChecksum`: per-page integrity verification utility (FNV-1a 32-bit)
//   - `IncrementalBackup`: track modified pages between backup snapshots
//   - `BackupManifest`: metadata for an incremental backup snapshot

use std::collections::{HashMap, HashSet};

use super::pager::PAGE_SIZE;

// ── FNV-1a 32-bit constants (same as pager.rs) ────────────────────────
const FNV32_OFFSET_BASIS: u32 = 0x811c_9dc5;
const FNV32_PRIME: u32 = 0x0100_0193;

// ── Page Checksum ─────────────────────────────────────────────────────

/// Compute FNV-1a 32-bit hash of a page buffer.
pub fn page_checksum(data: &[u8]) -> u32 {
    let mut h = FNV32_OFFSET_BASIS;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(FNV32_PRIME);
    }
    h
}

/// Per-page checksum registry — keeps a map of page_id → last-known checksum.
///
/// Used for:
///   1. Detecting page corruption after reads.
///   2. Tracking which pages were modified between snapshots.
#[derive(Debug, Clone)]
pub struct PageChecksumRegistry {
    checksums: HashMap<u32, u32>,
}

impl Default for PageChecksumRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PageChecksumRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            checksums: HashMap::new(),
        }
    }

    /// Register the checksum for a page.  Returns `true` if the page is new or
    /// its checksum changed (i.e. the page was modified).
    pub fn update(&mut self, page_id: u32, data: &[u8]) -> bool {
        let cs = page_checksum(data);
        let changed = self.checksums.get(&page_id) != Some(&cs);
        self.checksums.insert(page_id, cs);
        changed
    }

    /// Verify integrity of a page buffer against the stored checksum.
    /// Returns `None` if the page was never registered.
    pub fn verify(&self, page_id: u32, data: &[u8]) -> Option<bool> {
        self.checksums.get(&page_id).map(|&stored| {
            let actual = page_checksum(data);
            actual == stored
        })
    }

    /// Return the stored checksum for a page, if any.
    pub fn get(&self, page_id: u32) -> Option<u32> {
        self.checksums.get(&page_id).copied()
    }

    /// Number of tracked pages.
    pub fn len(&self) -> usize {
        self.checksums.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.checksums.is_empty()
    }

    /// Remove a page from the registry.
    pub fn remove(&mut self, page_id: u32) -> bool {
        self.checksums.remove(&page_id).is_some()
    }

    /// Clear all tracked checksums.
    pub fn clear(&mut self) {
        self.checksums.clear();
    }
}

// ── Incremental Backup ────────────────────────────────────────────────

/// Tracks page-level changes for incremental backup support.
///
/// Workflow:
///   1. Start a backup epoch with `begin_epoch()`.
///   2. Mark pages dirty via `mark_dirty(page_id)` as writes occur.
///   3. Call `snapshot()` to get the set of dirty pages and advance the epoch.
///   4. Copy only the dirty pages to the backup medium.
#[derive(Debug, Clone)]
pub struct IncrementalBackup {
    /// Current epoch number (monotonically increasing).
    epoch: u64,
    /// Pages dirtied in the current epoch.
    dirty: HashSet<u32>,
    /// History of past snapshots: epoch → set of dirty page IDs.
    history: Vec<BackupManifest>,
}

/// Metadata for a single incremental backup snapshot.
#[derive(Debug, Clone)]
pub struct BackupManifest {
    /// Epoch number.
    pub epoch: u64,
    /// Page IDs that were modified during this epoch.
    pub dirty_pages: Vec<u32>,
    /// Total database page count at snapshot time (caller-provided).
    pub total_pages: u32,
    /// Page size (constant, for validation).
    pub page_size: usize,
}

impl Default for IncrementalBackup {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalBackup {
    /// Create a new incremental backup tracker starting at epoch 0.
    pub fn new() -> Self {
        Self {
            epoch: 0,
            dirty: HashSet::new(),
            history: Vec::new(),
        }
    }

    /// Current epoch number.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Mark a page as modified in the current epoch.
    pub fn mark_dirty(&mut self, page_id: u32) {
        self.dirty.insert(page_id);
    }

    /// Number of dirty pages in the current epoch.
    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    /// Check if a specific page is dirty in the current epoch.
    pub fn is_dirty(&self, page_id: u32) -> bool {
        self.dirty.contains(&page_id)
    }

    /// Take a snapshot: record the current dirty set and advance the epoch.
    ///
    /// `total_pages` is the caller-provided total page count at this point.
    /// Returns the `BackupManifest` for this snapshot.
    pub fn snapshot(&mut self, total_pages: u32) -> BackupManifest {
        let mut dirty_pages: Vec<u32> = self.dirty.drain().collect();
        dirty_pages.sort_unstable();
        let manifest = BackupManifest {
            epoch: self.epoch,
            dirty_pages,
            total_pages,
            page_size: PAGE_SIZE,
        };
        self.history.push(manifest.clone());
        self.epoch += 1;
        manifest
    }

    /// Get all past manifests.
    pub fn history(&self) -> &[BackupManifest] {
        &self.history
    }

    /// Get the union of dirty pages across a range of epochs [from_epoch, to_epoch].
    ///
    /// Useful for merging multiple incremental backups into one.
    pub fn dirty_pages_in_range(&self, from_epoch: u64, to_epoch: u64) -> Vec<u32> {
        let mut merged: HashSet<u32> = HashSet::new();
        for m in &self.history {
            if m.epoch >= from_epoch && m.epoch <= to_epoch {
                for &p in &m.dirty_pages {
                    merged.insert(p);
                }
            }
        }
        let mut v: Vec<u32> = merged.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// Number of snapshots taken so far.
    pub fn snapshot_count(&self) -> usize {
        self.history.len()
    }

    /// Reset the tracker (clears history and resets epoch).
    pub fn reset(&mut self) {
        self.epoch = 0;
        self.dirty.clear();
        self.history.clear();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_checksum_deterministic() {
        let data = [0xAB_u8; PAGE_SIZE];
        let c1 = page_checksum(&data);
        let c2 = page_checksum(&data);
        assert_eq!(c1, c2, "checksum must be deterministic");
    }

    #[test]
    fn test_page_checksum_different_data() {
        let d1 = [0x00_u8; PAGE_SIZE];
        let d2 = [0xFF_u8; PAGE_SIZE];
        assert_ne!(page_checksum(&d1), page_checksum(&d2));
    }

    #[test]
    fn test_registry_update_new_page() {
        let mut reg = PageChecksumRegistry::new();
        let data = [0x42_u8; PAGE_SIZE];
        assert!(reg.update(1, &data), "new page should return true");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn test_registry_update_unchanged() {
        let mut reg = PageChecksumRegistry::new();
        let data = [0x42_u8; PAGE_SIZE];
        reg.update(1, &data);
        assert!(!reg.update(1, &data), "same data should return false");
    }

    #[test]
    fn test_registry_update_changed() {
        let mut reg = PageChecksumRegistry::new();
        let d1 = [0x42_u8; PAGE_SIZE];
        reg.update(1, &d1);
        let d2 = [0x43_u8; PAGE_SIZE];
        assert!(reg.update(1, &d2), "changed data should return true");
    }

    #[test]
    fn test_registry_verify_ok() {
        let mut reg = PageChecksumRegistry::new();
        let data = [0xAA_u8; PAGE_SIZE];
        reg.update(5, &data);
        assert_eq!(reg.verify(5, &data), Some(true));
    }

    #[test]
    fn test_registry_verify_corrupted() {
        let mut reg = PageChecksumRegistry::new();
        let data = [0xAA_u8; PAGE_SIZE];
        reg.update(5, &data);
        let bad = [0xBB_u8; PAGE_SIZE];
        assert_eq!(reg.verify(5, &bad), Some(false));
    }

    #[test]
    fn test_registry_verify_unknown_page() {
        let reg = PageChecksumRegistry::new();
        let data = [0x00_u8; PAGE_SIZE];
        assert_eq!(reg.verify(99, &data), None);
    }

    #[test]
    fn test_registry_remove() {
        let mut reg = PageChecksumRegistry::new();
        reg.update(1, &[0u8; PAGE_SIZE]);
        assert!(reg.remove(1));
        assert!(!reg.remove(1));
        assert!(reg.is_empty());
    }

    #[test]
    fn test_incremental_backup_mark_dirty() {
        let mut bk = IncrementalBackup::new();
        assert_eq!(bk.dirty_count(), 0);
        bk.mark_dirty(10);
        bk.mark_dirty(20);
        bk.mark_dirty(10); // duplicate
        assert_eq!(bk.dirty_count(), 2);
        assert!(bk.is_dirty(10));
        assert!(!bk.is_dirty(30));
    }

    #[test]
    fn test_incremental_backup_snapshot() {
        let mut bk = IncrementalBackup::new();
        bk.mark_dirty(3);
        bk.mark_dirty(1);
        bk.mark_dirty(2);
        let manifest = bk.snapshot(100);
        assert_eq!(manifest.epoch, 0);
        assert_eq!(manifest.dirty_pages, vec![1, 2, 3]); // sorted
        assert_eq!(manifest.total_pages, 100);
        assert_eq!(manifest.page_size, PAGE_SIZE);
        assert_eq!(bk.epoch(), 1);
        assert_eq!(bk.dirty_count(), 0); // cleared after snapshot
    }

    #[test]
    fn test_incremental_backup_multiple_epochs() {
        let mut bk = IncrementalBackup::new();
        bk.mark_dirty(1);
        bk.mark_dirty(2);
        bk.snapshot(50);

        bk.mark_dirty(2);
        bk.mark_dirty(3);
        bk.snapshot(55);

        assert_eq!(bk.snapshot_count(), 2);
        assert_eq!(bk.epoch(), 2);
        let merged = bk.dirty_pages_in_range(0, 1);
        assert_eq!(merged, vec![1, 2, 3]);
    }

    #[test]
    fn test_incremental_backup_partial_range() {
        let mut bk = IncrementalBackup::new();
        bk.mark_dirty(10);
        bk.snapshot(100); // epoch 0

        bk.mark_dirty(20);
        bk.snapshot(100); // epoch 1

        bk.mark_dirty(30);
        bk.snapshot(100); // epoch 2

        let r = bk.dirty_pages_in_range(1, 2);
        assert_eq!(r, vec![20, 30]);
    }

    #[test]
    fn test_incremental_backup_reset() {
        let mut bk = IncrementalBackup::new();
        bk.mark_dirty(1);
        bk.snapshot(10);
        bk.reset();
        assert_eq!(bk.epoch(), 0);
        assert_eq!(bk.snapshot_count(), 0);
        assert_eq!(bk.dirty_count(), 0);
    }

    #[test]
    fn test_page_checksum_empty_slice() {
        let cs = page_checksum(&[]);
        assert_eq!(cs, FNV32_OFFSET_BASIS);
    }
}
