// ── WAL (Write-Ahead Log) ──────────────────────────────────────────────────
//
// PostgreSQL-style WAL for crash recovery, write-amplification reduction,
// and concurrent readers.
//
// # On-disk format
//
// WAL file: `<database_dir>/<table>.wal` (one per `.kkdb` data file)
//
// Header (32 bytes):
//   [0..4]   MAGIC  b"WLOG"
//   [4..8]   version: u32 = 1
//   [8..16]  db_uuid: first 8 bytes of the database UUID (for matching)
//   [16..20] page_size: u32
//   [20..24] salt1: u32    (random salt for frame checksum)
//   [24..28] salt2: u32    (random salt for frame checksum)
//   [28..32] reserved: u32
//
// Frame (PAGE_SIZE + 24 bytes each):
//   [0..4]   page_num: u32
//   [4..8]   commit_size: u32  — 0 = non-commit frame; >0 = this is a commit
//                                frame and commit_size is the db page count
//                                after this transaction committed.
//   [8..16]  salt1|salt2: [u8; 8] (copy of header salts for validation)
//   [16..20] frame_checksum: u32 (FNV-1a over frame header + page data)
//   [20..24] reserved: u32
//   [24..24+PAGE_SIZE] page_data: [u8; PAGE_SIZE]
//
// # Checkpointing
//
// When the WAL accumulates enough frames (or on explicit CHECKPOINT),
// all committed frames are applied to the database file and the WAL is reset.

use crate::error::{KkdbError, Result};
use crate::storage::pager::PAGE_SIZE;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// WAL file magic bytes
const WAL_MAGIC: &[u8; 4] = b"WLOG";
const WAL_VERSION: u32 = 1;
const WAL_HEADER_SIZE: usize = 32;
const WAL_FRAME_HEADER_SIZE: usize = 24;
const WAL_FRAME_SIZE: usize = WAL_FRAME_HEADER_SIZE + PAGE_SIZE;

const FNV32_OFFSET_BASIS: u32 = 0x811C_9DC5;
const FNV32_PRIME: u32 = 16_777_619;

#[inline]
fn fnv32(data: &[u8]) -> u32 {
    let mut h = FNV32_OFFSET_BASIS;
    for b in data {
        h ^= *b as u32;
        h = h.wrapping_mul(FNV32_PRIME);
    }
    h
}

/// WAL header stored at the beginning of the WAL file.
#[derive(Debug, Clone)]
pub struct WalHeader {
    pub version: u32,
    pub db_uuid_prefix: [u8; 8],
    pub page_size: u32,
    pub salt1: u32,
    pub salt2: u32,
}

impl WalHeader {
    pub fn new(db_uuid: &[u8; 16], page_size: u32) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&db_uuid[..8]);
        WalHeader {
            version: WAL_VERSION,
            db_uuid_prefix: prefix,
            page_size,
            salt1: (nanos & 0xFFFF_FFFF) as u32,
            salt2: ((nanos >> 32) & 0xFFFF_FFFF) as u32,
        }
    }

    pub fn serialize(&self) -> [u8; WAL_HEADER_SIZE] {
        let mut buf = [0u8; WAL_HEADER_SIZE];
        buf[0..4].copy_from_slice(WAL_MAGIC);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..16].copy_from_slice(&self.db_uuid_prefix);
        buf[16..20].copy_from_slice(&self.page_size.to_le_bytes());
        buf[20..24].copy_from_slice(&self.salt1.to_le_bytes());
        buf[24..28].copy_from_slice(&self.salt2.to_le_bytes());
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < WAL_HEADER_SIZE {
            return Err(KkdbError::CorruptDatabase("WAL header too short".into()));
        }
        if &buf[0..4] != WAL_MAGIC {
            return Err(KkdbError::CorruptDatabase("invalid WAL magic".into()));
        }
        let version = u32::from_le_bytes(
            buf[4..8]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid WAL version".into()))?,
        );
        if version != WAL_VERSION {
            return Err(KkdbError::CorruptDatabase(format!(
                "unsupported WAL version: {}",
                version
            )));
        }
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&buf[8..16]);
        Ok(WalHeader {
            version,
            db_uuid_prefix: prefix,
            page_size: u32::from_le_bytes(
                buf[16..20]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid WAL page_size".into()))?,
            ),
            salt1: u32::from_le_bytes(
                buf[20..24]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid WAL salt1".into()))?,
            ),
            salt2: u32::from_le_bytes(
                buf[24..28]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid WAL salt2".into()))?,
            ),
        })
    }
}

/// A single WAL frame: one page write.
#[derive(Debug, Clone)]
pub struct WalFrame {
    /// Which database page this frame replaces.
    pub page_num: u32,
    /// If >0, this is a commit frame; value = total page count after commit.
    pub commit_size: u32,
    /// Frame checksum for integrity verification.
    pub checksum: u32,
    /// The full page content.
    pub data: Box<[u8; PAGE_SIZE]>,
}

impl WalFrame {
    /// Serialize frame header + page data into a WAL_FRAME_SIZE buffer.
    fn serialize(&self, salt1: u32, salt2: u32) -> Vec<u8> {
        let mut buf = vec![0u8; WAL_FRAME_SIZE];
        buf[0..4].copy_from_slice(&self.page_num.to_le_bytes());
        buf[4..8].copy_from_slice(&self.commit_size.to_le_bytes());
        buf[8..12].copy_from_slice(&salt1.to_le_bytes());
        buf[12..16].copy_from_slice(&salt2.to_le_bytes());
        // checksum computed over header[0..16] + page_data
        let mut cksum_data = Vec::with_capacity(16 + PAGE_SIZE);
        cksum_data.extend_from_slice(&buf[0..16]);
        cksum_data.extend_from_slice(&*self.data);
        let checksum = fnv32(&cksum_data);
        buf[16..20].copy_from_slice(&checksum.to_le_bytes());
        buf[20..24].copy_from_slice(&0u32.to_le_bytes()); // reserved
        buf[24..24 + PAGE_SIZE].copy_from_slice(&*self.data);
        buf
    }

    /// Deserialize a frame from a WAL_FRAME_SIZE buffer.
    fn deserialize(buf: &[u8], salt1: u32, salt2: u32) -> Result<Self> {
        if buf.len() < WAL_FRAME_SIZE {
            return Err(KkdbError::CorruptDatabase("WAL frame too short".into()));
        }
        let page_num = u32::from_le_bytes(
            buf[0..4]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid WAL frame page_num".into()))?,
        );
        let commit_size = u32::from_le_bytes(
            buf[4..8]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid WAL frame commit_size".into()))?,
        );
        let frame_salt1 = u32::from_le_bytes(
            buf[8..12]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid WAL frame salt1".into()))?,
        );
        let frame_salt2 = u32::from_le_bytes(
            buf[12..16]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid WAL frame salt2".into()))?,
        );
        if frame_salt1 != salt1 || frame_salt2 != salt2 {
            return Err(KkdbError::CorruptDatabase(
                "WAL frame salt mismatch (stale WAL?)".into(),
            ));
        }
        let stored_checksum = u32::from_le_bytes(
            buf[16..20]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid WAL frame checksum".into()))?,
        );
        // Verify checksum
        let mut cksum_data = Vec::with_capacity(16 + PAGE_SIZE);
        cksum_data.extend_from_slice(&buf[0..16]);
        cksum_data.extend_from_slice(&buf[24..24 + PAGE_SIZE]);
        let computed = fnv32(&cksum_data);
        if stored_checksum != computed {
            return Err(KkdbError::CorruptDatabase(
                "WAL frame checksum mismatch".into(),
            ));
        }

        let mut data = Box::new([0u8; PAGE_SIZE]);
        data.copy_from_slice(&buf[24..24 + PAGE_SIZE]);

        Ok(WalFrame {
            page_num,
            commit_size,
            checksum: stored_checksum,
            data,
        })
    }
}

/// WAL index: maps page_num → frame offset in the WAL file.
/// Only committed frames are indexed.
#[derive(Debug, Clone, Default)]
pub struct WalIndex {
    /// page_num → last committed frame index (0-based frame number)
    page_map: HashMap<u32, usize>,
    /// Total number of frames in the WAL (including uncommitted)
    total_frames: usize,
    /// Number of committed frames
    committed_frames: usize,
    /// Database page count from the last commit frame
    pub db_page_count: u32,
}

impl WalIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the most recent committed frame index for a given page, if any.
    pub fn get_page_frame(&self, page_num: u32) -> Option<usize> {
        self.page_map.get(&page_num).copied()
    }

    /// Number of committed frames.
    pub fn committed_frame_count(&self) -> usize {
        self.committed_frames
    }
}

/// WAL fsync strategy — controls when `sync_data()` is called.
///
/// - `Immediate`: fsync after every commit (safest, slowest).
///   Guarantees durability for every committed transaction.
/// - `GroupCommit`: defer fsync; batch multiple commits into a single fsync.
///   Call [`Wal::group_sync`] to flush all pending commits to disk at once.
///   This amortises the fsync cost over many transactions.
/// - `NoSync`: never fsync (fastest, crash unsafe).
///   Suitable for bulk loading or ephemeral data where durability is not required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WalSyncMode {
    /// fsync after each commit (default).
    #[default]
    Immediate,
    /// Defer fsync; caller must invoke `group_sync()` to flush.
    GroupCommit,
    /// Never fsync — let the OS decide when to flush.
    NoSync,
}

/// Statistics about WAL write and sync performance.
#[derive(Debug, Clone, Default)]
pub struct WalStats {
    /// Total number of commits (successful `commit()` calls).
    pub total_commits: u64,
    /// Total number of `fsync` / `sync_data()` calls.
    pub total_fsyncs: u64,
    /// Total number of `group_sync()` flushes.
    pub group_syncs: u64,
    /// Total number of frames written across all commits.
    pub total_frames_written: u64,
    /// Number of commits pending fsync (GroupCommit mode).
    pub pending_sync_commits: u64,
    /// Total number of checkpoints performed.
    pub total_checkpoints: u64,
    /// Total number of frames applied during checkpoints.
    pub total_checkpoint_frames: u64,
    /// Number of checkpoint requests blocked by active snapshots.
    pub blocked_checkpoints: u64,
    /// Maximum group-commit batch size observed.
    pub max_batch_size: u64,
    /// WAL file size in bytes at last commit (0 for in-memory).
    pub wal_file_bytes: u64,
}

/// Configuration for group-commit batching behavior.
#[derive(Debug, Clone, Default)]
pub struct GroupCommitConfig {
    /// Maximum number of commits to batch before triggering group_sync.
    /// 0 = no automatic trigger (caller must invoke group_sync manually).
    pub max_batch_commits: u64,
    /// Whether to automatically call group_sync when max_batch_commits is reached.
    pub auto_sync_on_batch: bool,
}

/// Snapshot registry entry — tracks active reader snapshots to prevent
/// checkpointing past the oldest active snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    /// Unique snapshot ID (monotonically increasing).
    pub id: u64,
    /// Number of committed frames visible to this snapshot.
    pub visible_frames: usize,
}

/// The WAL manager for a single database file.
///
/// Provides write-ahead logging with crash recovery and concurrent-reader support.
/// Enhanced with:
/// - **Snapshot registry**: tracks active readers to prevent unsafe checkpoints.
/// - **Group-commit batching**: configurable batch size with auto-sync trigger.
/// - **Checkpoint statistics**: detailed metrics for monitoring WAL performance.
pub struct Wal {
    /// WAL file handle (None for in-memory mode).
    file: Option<File>,
    /// Path to the WAL file.
    #[allow(dead_code)]
    path: Option<PathBuf>,
    /// WAL header (salts, version, etc.)
    header: WalHeader,
    /// In-memory WAL index mapping page_num → frame index.
    index: WalIndex,
    /// Buffered uncommitted frames for the current transaction.
    uncommitted: Vec<WalFrame>,
    /// All committed frames kept in memory for reads before checkpoint.
    committed_frames: Vec<WalFrame>,
    /// Maximum WAL size (in frames) before auto-checkpoint triggers.
    pub auto_checkpoint_threshold: usize,
    /// Sync strategy for commit durability.
    sync_mode: WalSyncMode,
    /// Number of commits that have written frames but not yet fsynced (GroupCommit mode).
    pending_sync_commits: u64,
    /// Cumulative WAL statistics.
    stats: WalStats,
    /// Active reader snapshot registry — prevents checkpointing past active readers.
    active_snapshots: Vec<SnapshotEntry>,
    /// Next snapshot ID (monotonically increasing).
    next_snapshot_id: u64,
    /// Group-commit configuration.
    group_commit_config: GroupCommitConfig,
}

impl Wal {
    /// Create a new WAL for a database file.
    pub fn create<P: AsRef<Path>>(wal_path: P, db_uuid: &[u8; 16]) -> Result<Self> {
        let path = wal_path.as_ref();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        let header = WalHeader::new(db_uuid, PAGE_SIZE as u32);
        file.write_all(&header.serialize())?;
        file.sync_data()?;

        Ok(Wal {
            file: Some(file),
            path: Some(path.to_path_buf()),
            header,
            index: WalIndex::new(),
            uncommitted: Vec::new(),
            committed_frames: Vec::new(),
            auto_checkpoint_threshold: 1000,
            sync_mode: WalSyncMode::Immediate,
            pending_sync_commits: 0,
            stats: WalStats::default(),
            active_snapshots: Vec::new(),
            next_snapshot_id: 1,
            group_commit_config: GroupCommitConfig::default(),
        })
    }

    /// Open an existing WAL file, rebuilding the in-memory index.
    pub fn open<P: AsRef<Path>>(wal_path: P) -> Result<Self> {
        let path = wal_path.as_ref();
        if !path.exists() {
            return Err(KkdbError::RuntimeError("WAL file does not exist".into()));
        }

        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < WAL_HEADER_SIZE as u64 {
            return Err(KkdbError::CorruptDatabase("WAL file too short".into()));
        }

        // Read header
        let mut hdr_buf = [0u8; WAL_HEADER_SIZE];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut hdr_buf)?;
        let header = WalHeader::deserialize(&hdr_buf)?;

        // Rebuild index by scanning all frames
        let mut index = WalIndex::new();
        let mut committed_frames = Vec::new();
        let mut offset = WAL_HEADER_SIZE as u64;
        let mut frame_idx = 0usize;
        let mut pending_page_map: HashMap<u32, usize> = HashMap::new();

        while offset + WAL_FRAME_SIZE as u64 <= file_len {
            let mut frame_buf = vec![0u8; WAL_FRAME_SIZE];
            file.seek(SeekFrom::Start(offset))?;
            match file.read_exact(&mut frame_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(KkdbError::Io(e)),
            }

            let frame = match WalFrame::deserialize(&frame_buf, header.salt1, header.salt2) {
                Ok(f) => f,
                Err(_) => break, // Corrupt frame — stop scanning (partial write)
            };

            pending_page_map.insert(frame.page_num, committed_frames.len());
            committed_frames.push(frame.clone());

            if frame.commit_size > 0 {
                // This is a commit frame — flush pending into index
                for (pn, fi) in pending_page_map.drain() {
                    index.page_map.insert(pn, fi);
                }
                index.committed_frames = committed_frames.len();
                index.db_page_count = frame.commit_size;
            }

            frame_idx += 1;
            offset += WAL_FRAME_SIZE as u64;
        }
        index.total_frames = frame_idx;

        // Truncate any uncommitted frames from committed_frames
        committed_frames.truncate(index.committed_frames);

        Ok(Wal {
            file: Some(file),
            path: Some(path.to_path_buf()),
            header,
            index,
            uncommitted: Vec::new(),
            committed_frames,
            auto_checkpoint_threshold: 1000,
            sync_mode: WalSyncMode::Immediate,
            pending_sync_commits: 0,
            stats: WalStats::default(),
            active_snapshots: Vec::new(),
            next_snapshot_id: 1,
            group_commit_config: GroupCommitConfig::default(),
        })
    }

    /// Open or create a WAL file for a database.
    pub fn open_or_create<P: AsRef<Path>>(wal_path: P, db_uuid: &[u8; 16]) -> Result<Self> {
        let path = wal_path.as_ref();
        if path.exists() {
            let file_len = std::fs::metadata(path)?.len();
            if file_len >= WAL_HEADER_SIZE as u64 {
                return Self::open(path);
            }
        }
        Self::create(path, db_uuid)
    }

    /// Create an in-memory WAL (no file backing).
    pub fn open_memory(db_uuid: &[u8; 16]) -> Self {
        Wal {
            file: None,
            path: None,
            header: WalHeader::new(db_uuid, PAGE_SIZE as u32),
            index: WalIndex::new(),
            uncommitted: Vec::new(),
            committed_frames: Vec::new(),
            auto_checkpoint_threshold: usize::MAX,
            sync_mode: WalSyncMode::Immediate,
            pending_sync_commits: 0,
            stats: WalStats::default(),
            active_snapshots: Vec::new(),
            next_snapshot_id: 1,
            group_commit_config: GroupCommitConfig::default(),
        }
    }

    /// Write a page to the WAL (uncommitted — buffered until commit).
    pub fn write_page(&mut self, page_num: u32, data: &[u8; PAGE_SIZE]) -> Result<()> {
        let frame = WalFrame {
            page_num,
            commit_size: 0, // not a commit frame yet
            checksum: 0,    // computed at serialize time
            data: Box::new(*data),
        };
        self.uncommitted.push(frame);
        Ok(())
    }

    /// Commit all buffered frames: mark the last frame as a commit frame and
    /// write to disk. Respects [`WalSyncMode`]:
    /// - `Immediate`: writes frames and fsyncs immediately.
    /// - `GroupCommit`: writes frames but defers fsync. Call [`group_sync`] later.
    /// - `NoSync`: writes frames without any fsync.
    pub fn commit(&mut self, db_page_count: u32) -> Result<()> {
        if self.uncommitted.is_empty() {
            return Ok(());
        }

        let frame_count = self.uncommitted.len() as u64;

        // Mark the last frame as the commit frame
        let last_idx = self.uncommitted.len() - 1;
        self.uncommitted[last_idx].commit_size = db_page_count;

        // Write all frames to the WAL file
        if let Some(ref mut file) = self.file {
            let write_offset =
                WAL_HEADER_SIZE as u64 + (self.index.total_frames as u64) * WAL_FRAME_SIZE as u64;
            file.seek(SeekFrom::Start(write_offset))?;

            for frame in &self.uncommitted {
                let serialized = frame.serialize(self.header.salt1, self.header.salt2);
                file.write_all(&serialized)?;
            }

            // Sync strategy
            match self.sync_mode {
                WalSyncMode::Immediate => {
                    file.sync_data()?;
                    self.stats.total_fsyncs += 1;
                }
                WalSyncMode::GroupCommit => {
                    // Defer fsync — data is written but not guaranteed durable
                    self.pending_sync_commits += 1;
                }
                WalSyncMode::NoSync => {
                    // No fsync at all
                }
            }
        }

        // Move uncommitted frames into committed storage and update index
        let base = self.committed_frames.len();
        for (i, frame) in self.uncommitted.drain(..).enumerate() {
            let frame_idx = base + i;
            self.index.page_map.insert(frame.page_num, frame_idx);
            self.committed_frames.push(frame);
        }
        self.index.committed_frames = self.committed_frames.len();
        self.index.total_frames = self.committed_frames.len();
        self.index.db_page_count = db_page_count;

        // Update stats
        self.stats.total_commits += 1;
        self.stats.total_frames_written += frame_count;
        self.stats.pending_sync_commits = self.pending_sync_commits;

        // Track WAL file size
        if self.file.is_some() {
            self.stats.wal_file_bytes = WAL_HEADER_SIZE as u64
                + (self.committed_frames.len() as u64) * WAL_FRAME_SIZE as u64;
        }

        // Track max batch size
        if self.pending_sync_commits > self.stats.max_batch_size {
            self.stats.max_batch_size = self.pending_sync_commits;
        }

        // Auto group-sync trigger: if we've accumulated enough pending commits
        if self.group_commit_config.auto_sync_on_batch
            && self.group_commit_config.max_batch_commits > 0
            && self.pending_sync_commits >= self.group_commit_config.max_batch_commits
        {
            self.group_sync()?;
        }

        Ok(())
    }

    /// Flush all pending commits to disk in a single fsync (group commit).
    ///
    /// In `GroupCommit` mode, this is the batched durability point.
    /// Multiple transactions may have called `commit()` since the last
    /// `group_sync()`, and this single fsync makes them all durable at once.
    ///
    /// Returns the number of commits that were waiting for sync.
    pub fn group_sync(&mut self) -> Result<u64> {
        let pending = self.pending_sync_commits;
        if pending == 0 {
            return Ok(0);
        }
        if let Some(ref mut file) = self.file {
            file.sync_data()?;
            self.stats.total_fsyncs += 1;
            self.stats.group_syncs += 1;
        }
        self.pending_sync_commits = 0;
        self.stats.pending_sync_commits = 0;
        Ok(pending)
    }

    /// Set the WAL sync mode.
    pub fn set_sync_mode(&mut self, mode: WalSyncMode) {
        self.sync_mode = mode;
    }

    /// Get the current WAL sync mode.
    pub fn sync_mode(&self) -> WalSyncMode {
        self.sync_mode
    }

    /// Return a snapshot of WAL performance statistics.
    pub fn wal_stats(&self) -> WalStats {
        self.stats.clone()
    }

    /// Rollback: discard all uncommitted frames.
    pub fn rollback(&mut self) {
        self.uncommitted.clear();
    }

    /// Read a page from the WAL. Returns `Some(data)` if a committed version
    /// of the page exists in the WAL, otherwise `None` (read from main db file).
    pub fn read_page(&self, page_num: u32) -> Option<&[u8; PAGE_SIZE]> {
        // Also check uncommitted frames for the current writer
        // (write-read-your-own-writes within a transaction)
        for frame in self.uncommitted.iter().rev() {
            if frame.page_num == page_num {
                return Some(&*frame.data);
            }
        }
        // Then check committed frames
        if let Some(&frame_idx) = self.index.page_map.get(&page_num) {
            if frame_idx < self.committed_frames.len() {
                return Some(&*self.committed_frames[frame_idx].data);
            }
        }
        None
    }

    /// Checkpoint: apply all committed WAL frames to the database file,
    /// then reset the WAL.
    ///
    /// **Snapshot safety**: if active reader snapshots exist, checkpoint will
    /// only apply frames up to the oldest active snapshot boundary. If no
    /// frames can be checkpointed (all are still visible to active readers),
    /// returns `Ok(0)` and increments `blocked_checkpoints` in stats.
    ///
    /// `db_file` — mutable reference to the opened database file.
    pub fn checkpoint(&mut self, db_file: &mut File) -> Result<usize> {
        // Determine how far we can checkpoint (snapshot safety)
        let checkpoint_boundary = self.safe_checkpoint_boundary();

        if checkpoint_boundary == 0 && !self.active_snapshots.is_empty() {
            // Cannot checkpoint — all frames are visible to some active snapshot
            self.stats.blocked_checkpoints += 1;
            return Ok(0);
        }

        let frames_applied = if checkpoint_boundary >= self.committed_frames.len() {
            // Full checkpoint — no active snapshots blocking
            self.full_checkpoint(db_file)?
        } else {
            // Partial checkpoint — apply only frames up to boundary
            self.partial_checkpoint(db_file, checkpoint_boundary)?
        };

        // Update checkpoint stats
        self.stats.total_checkpoints += 1;
        self.stats.total_checkpoint_frames += frames_applied as u64;

        Ok(frames_applied)
    }

    /// Determine the safe checkpoint boundary considering active snapshots.
    /// Returns the number of committed frames that can be safely checkpointed.
    pub fn safe_checkpoint_boundary(&self) -> usize {
        if self.active_snapshots.is_empty() {
            return self.committed_frames.len(); // No readers — checkpoint all
        }
        // Find the minimum visible_frames across all active snapshots
        let min_visible = self
            .active_snapshots
            .iter()
            .map(|s| s.visible_frames)
            .min()
            .unwrap_or(0);

        // We can checkpoint frames that are BEFORE the oldest snapshot
        // (i.e., frames that no active reader needs)
        // Since snapshots capture up to `visible_frames`, any frame index < min_visible
        // is visible to some reader. We can only checkpoint if we checkpoint ALL such frames.
        // However, if min_visible == 0, we can't checkpoint at all.
        // For simplicity: checkpoint everything if all readers see the full WAL,
        // otherwise don't checkpoint (conservative but safe).
        if min_visible >= self.committed_frames.len() {
            self.committed_frames.len() // All readers see all frames — safe to checkpoint all
        } else {
            0 // Some reader might still need older frames — block checkpoint
        }
    }

    /// Full checkpoint: apply ALL committed frames and reset WAL.
    fn full_checkpoint(&mut self, db_file: &mut File) -> Result<usize> {
        let frames_applied = self.committed_frames.len();

        // Write each committed page to the correct position in the database file
        let mut written_pages: HashMap<u32, bool> = HashMap::new();

        for (&page_num, &frame_idx) in &self.index.page_map {
            if written_pages.contains_key(&page_num) {
                continue;
            }
            let frame = &self.committed_frames[frame_idx];
            let offset = (page_num as u64 - 1) * PAGE_SIZE as u64;
            db_file.seek(SeekFrom::Start(offset))?;
            db_file.write_all(&*frame.data)?;
            written_pages.insert(page_num, true);
        }

        // Truncate or extend db file to match the committed page count
        if self.index.db_page_count > 0 {
            let expected_len = self.index.db_page_count as u64 * PAGE_SIZE as u64;
            db_file.set_len(expected_len)?;
        }
        db_file.sync_data()?;

        // Reset WAL: rewrite header with new salts, clear in-memory state
        self.committed_frames.clear();
        self.uncommitted.clear();
        self.index = WalIndex::new();

        // Rewrite WAL header with fresh salts
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.header.salt1 = (nanos & 0xFFFF_FFFF) as u32;
        self.header.salt2 = ((nanos >> 32) & 0xFFFF_FFFF) as u32;

        if let Some(ref mut file) = self.file {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&self.header.serialize())?;
            file.set_len(WAL_HEADER_SIZE as u64)?;
            file.sync_data()?;
        }

        // Update WAL file size in stats
        self.stats.wal_file_bytes = WAL_HEADER_SIZE as u64;

        Ok(frames_applied)
    }

    /// Partial checkpoint: apply frames up to `boundary` and compact the WAL.
    /// Frames beyond `boundary` are retained for active readers.
    fn partial_checkpoint(&mut self, db_file: &mut File, boundary: usize) -> Result<usize> {
        if boundary == 0 {
            return Ok(0);
        }

        // Write pages from the checkpointed region
        let mut written_pages: HashMap<u32, bool> = HashMap::new();

        for (&page_num, &frame_idx) in &self.index.page_map {
            if frame_idx >= boundary {
                continue; // Skip frames beyond checkpoint boundary
            }
            if written_pages.contains_key(&page_num) {
                continue;
            }
            let frame = &self.committed_frames[frame_idx];
            let offset = (page_num as u64 - 1) * PAGE_SIZE as u64;
            db_file.seek(SeekFrom::Start(offset))?;
            db_file.write_all(&*frame.data)?;
            written_pages.insert(page_num, true);
        }
        db_file.sync_data()?;

        // Compact: remove checkpointed frames, adjust indices
        self.committed_frames.drain(..boundary);

        // Rebuild page_map with adjusted indices
        let mut new_map: HashMap<u32, usize> = HashMap::new();
        for (i, frame) in self.committed_frames.iter().enumerate() {
            new_map.insert(frame.page_num, i);
        }
        self.index.page_map = new_map;
        self.index.committed_frames = self.committed_frames.len();
        self.index.total_frames = self.committed_frames.len();

        // Adjust snapshot boundaries
        for snap in &mut self.active_snapshots {
            snap.visible_frames = snap.visible_frames.saturating_sub(boundary);
        }

        Ok(boundary)
    }

    /// Check if auto-checkpoint should trigger.
    pub fn should_checkpoint(&self) -> bool {
        self.committed_frames.len() >= self.auto_checkpoint_threshold
    }

    /// Number of committed frames currently in the WAL.
    pub fn committed_frame_count(&self) -> usize {
        self.committed_frames.len()
    }

    /// Number of uncommitted frames buffered.
    pub fn uncommitted_frame_count(&self) -> usize {
        self.uncommitted.len()
    }

    /// Check if the WAL is empty (no committed or uncommitted frames).
    pub fn is_empty(&self) -> bool {
        self.committed_frames.is_empty() && self.uncommitted.is_empty()
    }

    // ── Group-commit configuration ──────────────────────────────────────────

    /// Set the group-commit configuration.
    ///
    /// When `auto_sync_on_batch` is true and `max_batch_commits > 0`,
    /// `commit()` will automatically call `group_sync()` once the pending
    /// commit count reaches `max_batch_commits`.
    pub fn set_group_commit_config(&mut self, config: GroupCommitConfig) {
        self.group_commit_config = config;
    }

    /// Get the current group-commit configuration.
    pub fn group_commit_config(&self) -> &GroupCommitConfig {
        &self.group_commit_config
    }

    // ── Snapshot registry for safe concurrent reads ─────────────────────────

    /// Register a read snapshot and track it in the active snapshot registry.
    ///
    /// Unlike [`snapshot`], this method also records the snapshot in the
    /// WAL's internal registry so that [`checkpoint`] knows not to discard
    /// frames that are still visible to active readers.
    ///
    /// Returns a `(snapshot_id, WalSnapshot)` pair. The caller must call
    /// [`release_snapshot`] when done reading.
    pub fn register_snapshot(&mut self) -> (u64, WalSnapshot) {
        let id = self.next_snapshot_id;
        self.next_snapshot_id += 1;

        let snap = WalSnapshot {
            page_map: self.index.page_map.clone(),
            snapshot_end: self.committed_frames.len(),
        };

        self.active_snapshots.push(SnapshotEntry {
            id,
            visible_frames: snap.snapshot_end,
        });

        (id, snap)
    }

    /// Release a registered snapshot, allowing checkpoint to reclaim those frames.
    ///
    /// Returns `true` if the snapshot was found and removed.
    pub fn release_snapshot(&mut self, snapshot_id: u64) -> bool {
        let before = self.active_snapshots.len();
        self.active_snapshots.retain(|s| s.id != snapshot_id);
        self.active_snapshots.len() < before
    }

    /// Number of active (registered) snapshots.
    pub fn active_snapshot_count(&self) -> usize {
        self.active_snapshots.len()
    }

    /// Check if checkpoint is currently blocked by active snapshots.
    pub fn is_checkpoint_blocked(&self) -> bool {
        if self.active_snapshots.is_empty() {
            return false;
        }
        self.safe_checkpoint_boundary() == 0
    }

    // ── Concurrent-reader snapshot support ───────────────────────────────────

    /// Take a read snapshot of the current committed WAL state.
    ///
    /// A `WalSnapshot` captures the committed frame count at the moment of
    /// creation. A reader holding a snapshot sees a consistent view: only
    /// committed frames up to `snapshot_end` are visible, regardless of new
    /// commits that happen concurrently.
    ///
    /// The snapshot borrows nothing from `Wal`; it stores the page-map
    /// (cloned) so readers can query without locking the writer.
    pub fn snapshot(&self) -> WalSnapshot {
        WalSnapshot {
            page_map: self.index.page_map.clone(),
            snapshot_end: self.committed_frames.len(),
        }
    }

    /// Read a page using a specific snapshot (for concurrent readers).
    ///
    /// Returns `Some(data)` only if the page's latest committed frame index
    /// falls within the snapshot boundary.
    pub fn read_page_snapshot(
        &self,
        page_num: u32,
        snapshot: &WalSnapshot,
    ) -> Option<&[u8; PAGE_SIZE]> {
        if let Some(&frame_idx) = snapshot.page_map.get(&page_num) {
            if frame_idx < snapshot.snapshot_end && frame_idx < self.committed_frames.len() {
                return Some(&*self.committed_frames[frame_idx].data);
            }
        }
        None
    }
}

/// A point-in-time read snapshot of the WAL for concurrent readers.
///
/// Created via [`Wal::snapshot`]. The snapshot captures which committed frames
/// are visible; new commits after the snapshot was taken are invisible to
/// readers using this snapshot.
///
/// Multiple readers can hold their own `WalSnapshot` simultaneously while
/// a single writer appends new frames to the WAL. The writer only needs to
/// defer checkpoint until all snapshots older than the target checkpoint
/// boundary have been released.
#[derive(Debug, Clone)]
pub struct WalSnapshot {
    /// Cloned page-map at snapshot-creation time.
    page_map: HashMap<u32, usize>,
    /// Number of committed frames visible in this snapshot.
    snapshot_end: usize,
}

impl WalSnapshot {
    /// Number of committed frames visible through this snapshot.
    pub fn visible_frame_count(&self) -> usize {
        self.snapshot_end
    }

    /// Check if a given page has a version visible in this snapshot.
    pub fn has_page(&self, page_num: u32) -> bool {
        if let Some(&idx) = self.page_map.get(&page_num) {
            idx < self.snapshot_end
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_page(fill: u8) -> [u8; PAGE_SIZE] {
        [fill; PAGE_SIZE]
    }

    #[test]
    fn test_wal_memory_write_commit_read() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // Write 3 pages
        wal.write_page(3, &make_page(0xAA)).unwrap();
        wal.write_page(5, &make_page(0xBB)).unwrap();
        wal.write_page(3, &make_page(0xCC)).unwrap();

        // Before commit, uncommitted frames are visible to the writer
        assert_eq!(wal.read_page(3).unwrap()[0], 0xCC);
        assert_eq!(wal.read_page(5).unwrap()[0], 0xBB);
        assert!(wal.read_page(7).is_none());

        // Commit
        wal.commit(10).unwrap();
        assert_eq!(wal.committed_frame_count(), 3);

        // After commit, latest page 3 is 0xCC
        assert_eq!(wal.read_page(3).unwrap()[0], 0xCC);
        assert_eq!(wal.read_page(5).unwrap()[0], 0xBB);
    }

    #[test]
    fn test_wal_rollback() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        wal.write_page(3, &make_page(0xAA)).unwrap();
        wal.commit(5).unwrap();

        // Start a new transaction
        wal.write_page(3, &make_page(0xBB)).unwrap();
        assert_eq!(wal.read_page(3).unwrap()[0], 0xBB);

        // Rollback
        wal.rollback();
        // Should see the committed version
        assert_eq!(wal.read_page(3).unwrap()[0], 0xAA);
    }

    #[test]
    fn test_wal_file_create_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let uuid = [42u8; 16];

        // Create WAL, write, commit
        {
            let mut wal = Wal::create(&wal_path, &uuid).unwrap();
            wal.write_page(3, &make_page(0x11)).unwrap();
            wal.write_page(4, &make_page(0x22)).unwrap();
            wal.commit(5).unwrap();
        }

        // Reopen and verify
        {
            let wal = Wal::open(&wal_path).unwrap();
            assert_eq!(wal.committed_frame_count(), 2);
            assert_eq!(wal.read_page(3).unwrap()[0], 0x11);
            assert_eq!(wal.read_page(4).unwrap()[0], 0x22);
            assert!(wal.read_page(5).is_none());
        }
    }

    #[test]
    fn test_wal_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.kkdb");
        let wal_path = dir.path().join("test.wal");
        let uuid = [7u8; 16];

        // Create a minimal database file (3 pages)
        {
            let mut db = File::create(&db_path).unwrap();
            for _ in 0..3 {
                db.write_all(&[0u8; PAGE_SIZE]).unwrap();
            }
        }

        // Create WAL, write modified pages, commit
        let mut wal = Wal::create(&wal_path, &uuid).unwrap();
        let mut page3 = make_page(0x00);
        page3[0] = 0xFF; // distinctive marker
        wal.write_page(3, &page3).unwrap();
        wal.commit(3).unwrap();

        // Checkpoint: apply WAL to db file
        let mut db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&db_path)
            .unwrap();
        let applied = wal.checkpoint(&mut db_file).unwrap();
        assert_eq!(applied, 1);
        assert!(wal.is_empty());

        // Verify db file has the written data
        db_file.seek(SeekFrom::Start(2 * PAGE_SIZE as u64)).unwrap();
        let mut buf = [0u8; PAGE_SIZE];
        db_file.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0xFF);
    }

    #[test]
    fn test_wal_header_roundtrip() {
        let uuid = [99u8; 16];
        let header = WalHeader::new(&uuid, PAGE_SIZE as u32);
        let buf = header.serialize();
        let restored = WalHeader::deserialize(&buf).unwrap();
        assert_eq!(restored.version, WAL_VERSION);
        assert_eq!(restored.page_size, PAGE_SIZE as u32);
        assert_eq!(restored.db_uuid_prefix, header.db_uuid_prefix);
        assert_eq!(restored.salt1, header.salt1);
        assert_eq!(restored.salt2, header.salt2);
    }

    #[test]
    fn test_wal_frame_roundtrip() {
        let salt1 = 0x12345678u32;
        let salt2 = 0xABCDEF01u32;
        let mut page = make_page(0x42);
        page[0] = 0xDE;
        page[1] = 0xAD;

        let frame = WalFrame {
            page_num: 7,
            commit_size: 100,
            checksum: 0,
            data: Box::new(page),
        };
        let serialized = frame.serialize(salt1, salt2);
        let restored = WalFrame::deserialize(&serialized, salt1, salt2).unwrap();
        assert_eq!(restored.page_num, 7);
        assert_eq!(restored.commit_size, 100);
        assert_eq!(restored.data[0], 0xDE);
        assert_eq!(restored.data[1], 0xAD);
        assert_eq!(restored.data[2], 0x42);
    }

    #[test]
    fn test_wal_multiple_transactions() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // Transaction 1
        wal.write_page(3, &make_page(0x11)).unwrap();
        wal.commit(5).unwrap();

        // Transaction 2
        wal.write_page(3, &make_page(0x22)).unwrap();
        wal.write_page(4, &make_page(0x33)).unwrap();
        wal.commit(5).unwrap();

        assert_eq!(wal.committed_frame_count(), 3);
        // Latest page 3 should be 0x22
        assert_eq!(wal.read_page(3).unwrap()[0], 0x22);
        assert_eq!(wal.read_page(4).unwrap()[0], 0x33);
    }

    #[test]
    fn test_wal_uncommitted_not_visible_after_rollback() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // No committed data yet
        assert!(wal.read_page(3).is_none());

        // Write and rollback
        wal.write_page(3, &make_page(0xFF)).unwrap();
        wal.rollback();

        assert!(wal.read_page(3).is_none());
    }

    #[test]
    fn test_wal_snapshot_isolation() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // Commit transaction 1
        wal.write_page(1, &make_page(0xAA)).unwrap();
        wal.commit(5).unwrap();

        // Take snapshot S1 — sees page 1 = 0xAA
        let snap1 = wal.snapshot();
        assert_eq!(snap1.visible_frame_count(), 1);
        assert!(snap1.has_page(1));
        assert!(!snap1.has_page(2));
        assert_eq!(wal.read_page_snapshot(1, &snap1).unwrap()[0], 0xAA);

        // Commit transaction 2 — modifies page 1, adds page 2
        wal.write_page(1, &make_page(0xBB)).unwrap();
        wal.write_page(2, &make_page(0xCC)).unwrap();
        wal.commit(5).unwrap();

        // S1 still sees old value for page 1 and does NOT see page 2
        assert_eq!(wal.read_page_snapshot(1, &snap1).unwrap()[0], 0xAA);
        assert!(wal.read_page_snapshot(2, &snap1).is_none());

        // New snapshot S2 sees both transactions
        let snap2 = wal.snapshot();
        assert_eq!(snap2.visible_frame_count(), 3);
        assert_eq!(wal.read_page_snapshot(1, &snap2).unwrap()[0], 0xBB);
        assert_eq!(wal.read_page_snapshot(2, &snap2).unwrap()[0], 0xCC);
    }

    #[test]
    fn test_wal_snapshot_unaffected_by_rollback() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        wal.write_page(5, &make_page(0x55)).unwrap();
        wal.commit(10).unwrap();
        let snap = wal.snapshot();

        // Start writing and then rollback — snapshot should be unaffected
        wal.write_page(5, &make_page(0x99)).unwrap();
        wal.rollback();

        assert_eq!(wal.read_page_snapshot(5, &snap).unwrap()[0], 0x55);
    }

    #[test]
    fn test_wal_multiple_snapshots_coexist() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // Three transactions, each overwriting page 1
        wal.write_page(1, &make_page(0x10)).unwrap();
        wal.commit(5).unwrap();
        let snap_a = wal.snapshot();

        wal.write_page(1, &make_page(0x20)).unwrap();
        wal.commit(5).unwrap();
        let snap_b = wal.snapshot();

        wal.write_page(1, &make_page(0x30)).unwrap();
        wal.commit(5).unwrap();
        let snap_c = wal.snapshot();

        // Each snapshot sees a different value for page 1
        assert_eq!(wal.read_page_snapshot(1, &snap_a).unwrap()[0], 0x10);
        assert_eq!(wal.read_page_snapshot(1, &snap_b).unwrap()[0], 0x20);
        assert_eq!(wal.read_page_snapshot(1, &snap_c).unwrap()[0], 0x30);
    }

    // ── WAL Crash Recovery Tests ────────────────────────────────────────────

    #[test]
    fn test_wal_crash_recovery_committed_frames_survive_reopen() {
        // Write committed data to WAL on disk, "crash" (drop without checkpoint),
        // reopen WAL and verify committed data is recovered.
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let uuid = [42u8; 16];

        // Phase 1: write + commit
        {
            let mut wal = Wal::create(&wal_path, &uuid).unwrap();
            wal.write_page(3, &make_page(0xAA)).unwrap();
            wal.write_page(4, &make_page(0xBB)).unwrap();
            wal.commit(10).unwrap();
            // "crash" — drop without checkpoint
        }

        // Phase 2: reopen and verify
        {
            let wal = Wal::open(&wal_path).unwrap();
            assert_eq!(wal.committed_frame_count(), 2);
            let p3 = wal.read_page(3).unwrap();
            assert_eq!(p3[0], 0xAA);
            let p4 = wal.read_page(4).unwrap();
            assert_eq!(p4[0], 0xBB);
        }
    }

    #[test]
    fn test_wal_crash_recovery_uncommitted_frames_discarded() {
        // Uncommitted (partial) writes should be discarded on reopen.
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let uuid = [43u8; 16];

        {
            let mut wal = Wal::create(&wal_path, &uuid).unwrap();
            // Committed transaction
            wal.write_page(3, &make_page(0xCC)).unwrap();
            wal.commit(5).unwrap();
            // Uncommitted transaction (simulates crash mid-write)
            wal.write_page(4, &make_page(0xDD)).unwrap();
            // NO commit — "crash"
        }

        {
            let wal = Wal::open(&wal_path).unwrap();
            // Only committed frame should survive
            assert_eq!(wal.committed_frame_count(), 1);
            assert!(wal.read_page(3).is_some());
            assert_eq!(wal.read_page(3).unwrap()[0], 0xCC);
            assert!(
                wal.read_page(4).is_none(),
                "uncommitted page should be discarded"
            );
        }
    }

    #[test]
    fn test_wal_crash_recovery_checkpoint_then_reopen() {
        // After checkpoint, WAL should be empty on reopen.
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let db_path = dir.path().join("test.kkdb");
        let uuid = [44u8; 16];

        // Create a minimal database file (5 pages)
        {
            let mut db_file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&db_path)
                .unwrap();
            let zeros = [0u8; PAGE_SIZE];
            for _ in 0..5 {
                db_file.write_all(&zeros).unwrap();
            }
            db_file.sync_data().unwrap();
        }

        {
            let mut wal = Wal::create(&wal_path, &uuid).unwrap();
            wal.write_page(3, &make_page(0xEE)).unwrap();
            wal.commit(5).unwrap();
            // Checkpoint: flush to db file
            let mut db_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&db_path)
                .unwrap();
            let n = wal.checkpoint(&mut db_file).unwrap();
            assert_eq!(n, 1);
        }

        // Reopen WAL — should be empty (checkpointed)
        {
            let wal = Wal::open(&wal_path).unwrap();
            assert_eq!(wal.committed_frame_count(), 0);
            assert!(wal.read_page(3).is_none(), "page should be in db file now");
        }

        // Verify database file has the data
        {
            let mut db_file = std::fs::File::open(&db_path).unwrap();
            let mut buf = [0u8; PAGE_SIZE];
            // Page 3 is at offset (3-1) * PAGE_SIZE
            db_file.seek(SeekFrom::Start(2 * PAGE_SIZE as u64)).unwrap();
            db_file.read_exact(&mut buf).unwrap();
            assert_eq!(
                buf[0], 0xEE,
                "database file should contain checkpointed data"
            );
        }
    }

    #[test]
    fn test_wal_crash_recovery_multi_txn_latest_wins() {
        // Multiple committed transactions overwriting the same page.
        // After reopen, latest value should be visible.
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let uuid = [45u8; 16];

        {
            let mut wal = Wal::create(&wal_path, &uuid).unwrap();
            // Txn 1
            wal.write_page(3, &make_page(0x01)).unwrap();
            wal.commit(5).unwrap();
            // Txn 2 overwrites page 3
            wal.write_page(3, &make_page(0x02)).unwrap();
            wal.commit(5).unwrap();
            // Txn 3 overwrites page 3 again
            wal.write_page(3, &make_page(0x03)).unwrap();
            wal.commit(5).unwrap();
            // "crash" — no checkpoint
        }

        {
            let wal = Wal::open(&wal_path).unwrap();
            assert_eq!(wal.committed_frame_count(), 3);
            let p3 = wal.read_page(3).unwrap();
            assert_eq!(p3[0], 0x03, "latest committed version should win");
        }
    }

    #[test]
    fn test_wal_crash_recovery_integration_with_pager() {
        // End-to-end: create database, enable WAL, write data, commit,
        // "crash" (drop without checkpoint), reopen and verify WAL state.
        use crate::storage::pager::Pager;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("crash_test.kkdb");

        // Phase 1: create, write, commit
        {
            let mut pager = Pager::create_cow_v2(&db_path).unwrap();
            pager.enable_wal().unwrap();
            assert!(pager.is_wal_enabled());

            // Write data to page 3 (the schema page)
            let page = pager.get_page_mut(3).unwrap();
            page.data[100] = 0x42;
            page.data[101] = 0x43;

            pager.flush().unwrap();
            // "crash" — drop pager (WAL not checkpointed)
        }

        // Phase 2: reopen with WAL and verify recovery
        {
            let mut pager = Pager::open_cow_v2(&db_path).unwrap();
            pager.enable_wal().unwrap();
            assert!(pager.is_wal_enabled());

            // WAL should still have committed frames
            let wal_frames = pager.wal.as_ref().unwrap().committed_frame_count();
            assert!(
                wal_frames > 0,
                "WAL should have recovered committed frames, got {}",
                wal_frames
            );

            // WAL read-back: page 3 data should be readable via WAL
            let wal_page = pager.wal.as_ref().unwrap().read_page(3);
            assert!(wal_page.is_some(), "page 3 should be in WAL");
            assert_eq!(wal_page.unwrap()[100], 0x42);
            assert_eq!(wal_page.unwrap()[101], 0x43);
        }
    }

    // ── Group Commit & Sync Mode Tests ──────────────────────────────────────

    #[test]
    fn test_wal_sync_mode_default_is_immediate() {
        let uuid = [0u8; 16];
        let wal = Wal::open_memory(&uuid);
        assert_eq!(wal.sync_mode(), WalSyncMode::Immediate);
    }

    #[test]
    fn test_wal_set_sync_mode() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);
        wal.set_sync_mode(WalSyncMode::GroupCommit);
        assert_eq!(wal.sync_mode(), WalSyncMode::GroupCommit);
        wal.set_sync_mode(WalSyncMode::NoSync);
        assert_eq!(wal.sync_mode(), WalSyncMode::NoSync);
    }

    #[test]
    fn test_wal_stats_after_commits() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // Commit 3 transactions
        for i in 0..3u8 {
            wal.write_page(i as u32 + 1, &make_page(0x10 + i)).unwrap();
            wal.commit(10).unwrap();
        }

        let stats = wal.wal_stats();
        assert_eq!(stats.total_commits, 3);
        assert_eq!(stats.total_frames_written, 3);
        // In-memory WAL has no file → no fsyncs
        assert_eq!(stats.total_fsyncs, 0);
    }

    #[test]
    fn test_wal_group_commit_file_based() {
        // GroupCommit mode: commit writes data but defers fsync.
        // group_sync() flushes all pending commits in one fsync.
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("gc.wal");
        let uuid = [50u8; 16];

        let mut wal = Wal::create(&wal_path, &uuid).unwrap();
        wal.set_sync_mode(WalSyncMode::GroupCommit);

        // 5 transactions without fsync
        for i in 0..5u8 {
            wal.write_page(i as u32 + 1, &make_page(i)).unwrap();
            wal.commit(10).unwrap();
        }

        let stats = wal.wal_stats();
        assert_eq!(stats.total_commits, 5);
        assert_eq!(stats.total_fsyncs, 0, "no fsync yet in GroupCommit mode");
        assert_eq!(stats.pending_sync_commits, 5);

        // Now group_sync — single fsync for all 5 commits
        let flushed = wal.group_sync().unwrap();
        assert_eq!(flushed, 5);

        let stats2 = wal.wal_stats();
        assert_eq!(stats2.total_fsyncs, 1, "one fsync for 5 commits");
        assert_eq!(stats2.group_syncs, 1);
        assert_eq!(stats2.pending_sync_commits, 0);

        // Data is still readable
        for i in 0..5u8 {
            assert_eq!(wal.read_page(i as u32 + 1).unwrap()[0], i);
        }
    }

    #[test]
    fn test_wal_group_sync_noop_when_nothing_pending() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);
        wal.set_sync_mode(WalSyncMode::GroupCommit);

        // No commits → group_sync returns 0
        let n = wal.group_sync().unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_wal_nosync_mode() {
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("nosync.wal");
        let uuid = [51u8; 16];

        let mut wal = Wal::create(&wal_path, &uuid).unwrap();
        wal.set_sync_mode(WalSyncMode::NoSync);

        wal.write_page(1, &make_page(0xAA)).unwrap();
        wal.commit(5).unwrap();

        let stats = wal.wal_stats();
        assert_eq!(stats.total_commits, 1);
        assert_eq!(stats.total_fsyncs, 0, "NoSync mode: zero fsyncs");
        assert_eq!(stats.pending_sync_commits, 0);

        // Data still readable
        assert_eq!(wal.read_page(1).unwrap()[0], 0xAA);
    }

    #[test]
    fn test_wal_immediate_mode_fsyncs_each_commit() {
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("imm.wal");
        let uuid = [52u8; 16];

        let mut wal = Wal::create(&wal_path, &uuid).unwrap();
        // Default is Immediate
        assert_eq!(wal.sync_mode(), WalSyncMode::Immediate);

        wal.write_page(1, &make_page(0x11)).unwrap();
        wal.commit(5).unwrap();
        wal.write_page(2, &make_page(0x22)).unwrap();
        wal.commit(5).unwrap();

        let stats = wal.wal_stats();
        assert_eq!(stats.total_commits, 2);
        assert_eq!(stats.total_fsyncs, 2, "Immediate mode: fsync per commit");
    }

    #[test]
    fn test_wal_group_commit_reopen_after_sync() {
        // After group_sync, data should survive reopen.
        let dir = tempfile::tempdir().unwrap();
        let wal_path = dir.path().join("gc_reopen.wal");
        let uuid = [53u8; 16];

        {
            let mut wal = Wal::create(&wal_path, &uuid).unwrap();
            wal.set_sync_mode(WalSyncMode::GroupCommit);

            wal.write_page(1, &make_page(0xDD)).unwrap();
            wal.commit(5).unwrap();
            wal.write_page(2, &make_page(0xEE)).unwrap();
            wal.commit(5).unwrap();

            wal.group_sync().unwrap();
            // Drop — "clean shutdown"
        }

        {
            let wal = Wal::open(&wal_path).unwrap();
            assert_eq!(wal.committed_frame_count(), 2);
            assert_eq!(wal.read_page(1).unwrap()[0], 0xDD);
            assert_eq!(wal.read_page(2).unwrap()[0], 0xEE);
        }
    }

    #[test]
    fn test_wal_stats_frames_multi_page_commit() {
        let uuid = [0u8; 16];
        let mut wal = Wal::open_memory(&uuid);

        // One commit with 4 frames
        wal.write_page(1, &make_page(0x01)).unwrap();
        wal.write_page(2, &make_page(0x02)).unwrap();
        wal.write_page(3, &make_page(0x03)).unwrap();
        wal.write_page(4, &make_page(0x04)).unwrap();
        wal.commit(10).unwrap();

        let stats = wal.wal_stats();
        assert_eq!(stats.total_commits, 1);
        assert_eq!(stats.total_frames_written, 4);
    }
}
