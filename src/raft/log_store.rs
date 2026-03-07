//! Persistent WAL-backed Raft log storage for KKDB.
//!
//! ## File layout
//!
//! ```text
//! {dir}/raft/
//!   wal.log       — append-only log of entries (CRC32-protected records)
//!   vote.json     — persisted Vote (overwritten on each save)
//!   purge.json    — persisted last_purged_log_id
//! ```
//!
//! ## WAL record format
//!
//! Each record is:
//!   [4 bytes LE: payload_len] [payload_len bytes: JSON] [4 bytes LE: crc32]
//!
//! On startup every record is replayed to rebuild the in-memory BTreeMap.
//! `truncate` / `purge` are done by:
//!   - `truncate` → re-writing the WAL without the truncated entries
//!   - `purge`    → dropping entries from memory + updating purge.json
//!                  (WAL compaction happens on next restart via replay-then-rewrite)

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use openraft::{
    Entry, LogId, LogState, OptionalSend, RaftLogReader, StorageError, StorageIOError, Vote,
    storage::{LogFlushed, RaftLogStorage},
};
use serde::{Deserialize, Serialize};

use crate::raft::types::{KkdbNodeId, KkdbTypeConfig};

// ─── WAL helpers ──────────────────────────────────────────────────────────────

/// Compute crc32 of a byte slice.
fn crc32(data: &[u8]) -> u32 {
    let mut c: u32 = 0xffff_ffff;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            if c & 1 != 0 { c = (c >> 1) ^ 0xEDB8_8320; } else { c >>= 1; }
        }
    }
    !c
}

fn write_record(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.write_all(&crc32(payload).to_le_bytes())?;
    Ok(())
}

fn read_record(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    let mut crc_buf = [0u8; 4];
    r.read_exact(&mut crc_buf)?;
    let recorded = u32::from_le_bytes(crc_buf);
    if crc32(&payload) != recorded {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL CRC mismatch"));
    }
    Ok(Some(payload))
}

// ─── Inner data ───────────────────────────────────────────────────────────────

pub struct KkdbLogStoreInner {
    pub last_purged_log_id: Option<LogId<KkdbNodeId>>,
    /// In-memory cache of unpurged log entries.
    pub log: BTreeMap<u64, Entry<KkdbTypeConfig>>,
    pub voted_for: Option<Vote<KkdbNodeId>>,
    /// WAL file (None = in-memory only).
    pub wal_file: Option<PathBuf>,
    /// Total records ever written to WAL (live + dead Truncate/purged Append).
    /// Used to decide when to compact.
    pub total_records: u64,
}

impl Default for KkdbLogStoreInner {
    fn default() -> Self {
        Self {
            last_purged_log_id: None,
            log: BTreeMap::new(),
            voted_for: None,
            wal_file: None,
            total_records: 0,
        }
    }
}

// ─── Public log store ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct KkdbLogStore {
    pub inner: Arc<Mutex<KkdbLogStoreInner>>,
}

/// Number of dead records (purged Appends + Truncate markers) in the WAL
/// before an automatic compaction is triggered.
const COMPACT_THRESHOLD: u64 = 1000;

impl KkdbLogStore {
    // ── Persistent constructor ─────────────────────────────────────────────

    /// Open (or create) a WAL-backed log store in `dir`.
    ///
    /// The directory structure created:
    ///   `dir/raft/wal.log`
    ///   `dir/raft/vote.json`
    ///   `dir/raft/purge.json`
    pub fn open(dir: &Path) -> io::Result<Self> {
        let raft_dir = dir.join("raft");
        fs::create_dir_all(&raft_dir)?;

        let wal_path = raft_dir.join("wal.log");
        let vote_path = raft_dir.join("vote.json");
        let purge_path = raft_dir.join("purge.json");

        // Recover vote
        let voted_for: Option<Vote<KkdbNodeId>> = if vote_path.exists() {
            let bytes = fs::read(&vote_path)?;
            serde_json::from_slice(&bytes).ok()
        } else {
            None
        };

        // Recover last_purged_log_id
        let last_purged: Option<LogId<KkdbNodeId>> = if purge_path.exists() {
            let bytes = fs::read(&purge_path)?;
            serde_json::from_slice(&bytes).ok()
        } else {
            None
        };

        // Replay WAL, counting total records for compaction heuristic
        let mut log: BTreeMap<u64, Entry<KkdbTypeConfig>> = BTreeMap::new();
        let mut total_records: u64 = 0;
        if wal_path.exists() {
            let f = File::open(&wal_path)?;
            let mut r = BufReader::new(f);
            loop {
                match read_record(&mut r) {
                    Ok(Some(payload)) => {
                        total_records += 1;
                        match serde_json::from_slice::<WalRecord>(&payload) {
                            Ok(WalRecord::Append(entry)) => {
                                log.insert(entry.log_id.index, entry);
                            }
                            Ok(WalRecord::Truncate { from_index }) => {
                                log.retain(|&k, _| k < from_index);
                            }
                            Err(_) => {} // skip corrupt record
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(_) => break,   // truncated tail — stop, rest is lost
                }
            }
            // Drop purged entries from replay
            if let Some(ref p) = last_purged {
                log.retain(|&k, _| k > p.index);
            }
        }

        let inner = KkdbLogStoreInner {
            last_purged_log_id: last_purged,
            log,
            voted_for,
            wal_file: Some(wal_path),
            total_records,
        };
        Ok(Self { inner: Arc::new(Mutex::new(inner)) })
    }

    // ── WAL compaction ─────────────────────────────────────────────────────────

    /// Rewrite `wal.log` keeping only entries that are **after**
    /// `last_purged_log_id` (i.e., the currently live in-memory entries).
    ///
    /// The rewrite is atomic: entries are written to `wal.new`, then the file
    /// is renamed over `wal.log`. On crash mid-rename the old WAL is preserved.
    ///
    /// Returns the number of dead records eliminated.
    pub fn compact(&self) -> io::Result<u64> {
        let mut inner = self.inner.lock().unwrap();
        let wal_path = match inner.wal_file {
            Some(ref p) => p.clone(),
            None => return Ok(0), // in-memory mode — nothing to compact
        };

        let live_count = inner.log.len() as u64;
        let dead = inner.total_records.saturating_sub(live_count);
        if dead == 0 {
            return Ok(0);
        }

        // Write only the live entries to a temporary file
        let tmp_path = wal_path.with_extension("new");
        {
            let f = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp_path)?;
            let mut w = BufWriter::new(f);
            for entry in inner.log.values() {
                let rec = WalRecord::Append(entry.clone());
                let payload = serde_json::to_vec(&rec)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                write_record(&mut w, &payload)?;
            }
            w.flush()?;
        }

        // Atomic rename
        fs::rename(&tmp_path, &wal_path)?;

        // Reset counter
        inner.total_records = live_count;

        Ok(dead)
    }

    /// Returns (live_records, total_records, dead_records) for diagnostic use.
    pub fn compaction_stats(&self) -> (u64, u64, u64) {
        let inner = self.inner.lock().unwrap();
        let live = inner.log.len() as u64;
        let total = inner.total_records;
        (live, total, total.saturating_sub(live))
    }

    // ── WAL write helpers (must hold the lock) ─────────────────────────────

    fn wal_append(wal_path: &Path, entry: &Entry<KkdbTypeConfig>) -> io::Result<()> {
        let rec = WalRecord::Append(entry.clone());
        let payload = serde_json::to_vec(&rec).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut f = OpenOptions::new().create(true).append(true).open(wal_path)?;
        {
            // Scope BufWriter so it is dropped (and its borrow released) before sync_data().
            let mut w = BufWriter::new(&mut f);
            write_record(&mut w, &payload)?;
            w.flush()?;
        }
        // S6 fix: fsync to guarantee durability before ACK-ing to Raft leader.
        // flush() only moves data to OS page cache; sync_data() ensures it reaches disk.
        f.sync_data()?;
        Ok(())
    }

    fn wal_truncate(wal_path: &Path, from_index: u64) -> io::Result<()> {
        let rec = WalRecord::Truncate { from_index };
        let payload = serde_json::to_vec(&rec).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut f = OpenOptions::new().create(true).append(true).open(wal_path)?;
        {
            let mut w = BufWriter::new(&mut f);
            write_record(&mut w, &payload)?;
            w.flush()?;
        }
        // S6 fix: fsync after truncate records as well
        f.sync_data()?;
        Ok(())
    }

    fn write_vote(wal_file: &Path, vote: &Vote<KkdbNodeId>) -> io::Result<()> {
        // I27 clarification: `wal_file` is the full path to wal.log.
        // The vote.json lives in the same directory (the raft/ dir), so we use .parent().
        // Renamed parameter from `dir` to `wal_file` to make the intent explicit.
        let raft_dir = wal_file.parent().unwrap_or(wal_file);
        let path = raft_dir.join("vote.json");
        let bytes = serde_json::to_vec(vote).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, bytes)
    }

    fn write_purge(wal_file: &Path, log_id: &LogId<KkdbNodeId>) -> io::Result<()> {
        // Same as write_vote: `wal_file` is the path to wal.log; parent() gives raft/ dir.
        let raft_dir = wal_file.parent().unwrap_or(wal_file);
        let path = raft_dir.join("purge.json");
        let bytes = serde_json::to_vec(log_id).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, bytes)
    }

    // ─── Test helpers ───────────────────────────────────────────────────────────

    /// Directly append entries to memory + WAL (bypasses LogFlushed for test use).
    pub fn append_direct(&self, entries: Vec<Entry<KkdbTypeConfig>>) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        for entry in entries {
            if let Some(ref wal) = inner.wal_file.clone() {
                Self::wal_append(wal, &entry)?;
                inner.total_records += 1;
            }
            inner.log.insert(entry.log_id.index, entry);
        }
        Ok(())
    }

    /// Directly truncate from `from_index` (for test use).
    pub fn truncate_direct(&self, from_index: u64) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(ref wal) = inner.wal_file.clone() {
            Self::wal_truncate(wal, from_index)?;
        }
        inner.log.retain(|&k, _| k < from_index);
        Ok(())
    }

    /// Directly purge up to `log_id` (for test use).
    pub fn purge_direct(&self, log_id: LogId<KkdbNodeId>) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        // treat purged entries as dead records for compaction tracking
        let purged = inner.log.range(..=log_id.index).count() as u64;
        inner.total_records += purged;
        inner.last_purged_log_id = Some(log_id);
        inner.log.retain(|&k, _| k > log_id.index);
        if let Some(ref wal) = inner.wal_file.clone() {
            Self::write_purge(wal, &log_id)?;
        }
        Ok(())
    }

    /// Read all log entries from the in-memory BTreeMap (for assertions).
    pub fn all_entries(&self) -> Vec<Entry<KkdbTypeConfig>> {
        self.inner.lock().unwrap().log.values().cloned().collect()
    }

    /// Count of entries in memory.
    pub fn entry_count(&self) -> usize {
        self.inner.lock().unwrap().log.len()
    }

    /// Last log index in memory.
    pub fn last_index(&self) -> Option<u64> {
        self.inner.lock().unwrap().log.keys().next_back().copied()
    }

    /// Last purged log id.
    pub fn last_purged(&self) -> Option<LogId<KkdbNodeId>> {
        self.inner.lock().unwrap().last_purged_log_id
    }

    /// Persisted vote.
    pub fn persisted_vote(&self) -> Option<Vote<KkdbNodeId>> {
        self.inner.lock().unwrap().voted_for
    }
}

// ─── WAL record enum ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
enum WalRecord {
    Append(Entry<KkdbTypeConfig>),
    Truncate { from_index: u64 },
}

// ─── Storage error helper ─────────────────────────────────────────────────────

fn io_to_storage_err(e: impl std::error::Error + 'static) -> StorageError<KkdbNodeId> {
    StorageIOError::write_logs(&e).into()
}

// ─── RaftLogReader ────────────────────────────────────────────────────────────

impl RaftLogReader<KkdbTypeConfig> for KkdbLogStore {
    async fn try_get_log_entries<RB>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<KkdbTypeConfig>>, StorageError<KkdbNodeId>>
    where
        RB: RangeBounds<u64> + Clone + Debug + OptionalSend,
    {
        let inner = self.inner.lock().unwrap();
        Ok(inner.log.range(range).map(|(_, e)| e.clone()).collect())
    }
}

// ─── RaftLogStorage ───────────────────────────────────────────────────────────

impl RaftLogStorage<KkdbTypeConfig> for KkdbLogStore {
    type LogReader = Self;

    async fn get_log_state(
        &mut self,
    ) -> Result<LogState<KkdbTypeConfig>, StorageError<KkdbNodeId>> {
        let inner = self.inner.lock().unwrap();
        let last = inner.log.values().next_back().map(|e| e.log_id);
        Ok(LogState {
            last_purged_log_id: inner.last_purged_log_id,
            last_log_id: last.or(inner.last_purged_log_id),
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(
        &mut self,
        vote: &Vote<KkdbNodeId>,
    ) -> Result<(), StorageError<KkdbNodeId>> {
        let mut inner = self.inner.lock().unwrap();
        inner.voted_for = Some(*vote);
        if let Some(ref wal) = inner.wal_file {
            Self::write_vote(wal, vote).map_err(io_to_storage_err)?;
        }
        Ok(())
    }

    async fn read_vote(
        &mut self,
    ) -> Result<Option<Vote<KkdbNodeId>>, StorageError<KkdbNodeId>> {
        Ok(self.inner.lock().unwrap().voted_for)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<KkdbTypeConfig>,
    ) -> Result<(), StorageError<KkdbNodeId>>
    where
        I: IntoIterator<Item = Entry<KkdbTypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let mut inner = self.inner.lock().unwrap();
        for entry in entries {
            if let Some(ref wal) = inner.wal_file {
                Self::wal_append(wal, &entry).map_err(io_to_storage_err)?;
                inner.total_records += 1;
            }
            inner.log.insert(entry.log_id.index, entry);
        }
        // Data is now fsynced to disk (or in-memory for tests)
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(
        &mut self,
        log_id: LogId<KkdbNodeId>,
    ) -> Result<(), StorageError<KkdbNodeId>> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(ref wal) = inner.wal_file.clone() {
            Self::wal_truncate(wal, log_id.index).map_err(io_to_storage_err)?;
            // Count the Truncate marker + the about-to-be-removed entries as dead
            let removed = inner.log.range(log_id.index..).count() as u64;
            inner.total_records += 1 + removed; // 1 for the Truncate record itself
        }
        inner.log.retain(|&k, _| k < log_id.index);
        Ok(())
    }

    async fn purge(
        &mut self,
        log_id: LogId<KkdbNodeId>,
    ) -> Result<(), StorageError<KkdbNodeId>> {
        let mut inner = self.inner.lock().unwrap();
        // Count entries being purged as dead records
        let purged_count = inner.log.range(..=log_id.index).count() as u64;
        inner.last_purged_log_id = Some(log_id);
        inner.log.retain(|&k, _| k > log_id.index);
        if let Some(ref wal) = inner.wal_file.clone() {
            Self::write_purge(wal, &log_id).map_err(io_to_storage_err)?;
        }

        // Auto-compact when dead records exceed threshold
        let live = inner.log.len() as u64;
        let dead = inner.total_records.saturating_sub(live) + purged_count;
        drop(inner); // release lock before compact() which re-acquires it
        if dead > COMPACT_THRESHOLD {
            let _ = self.compact(); // best-effort: ignore errors (WAL still valid)
        }
        Ok(())
    }
}
