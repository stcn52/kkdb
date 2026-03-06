use crate::error::{KkdbError, Result};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Page size in bytes (4KB, same as SQLite default)
pub const PAGE_SIZE: usize = 4096;

/// Maximum number of pages (limits DB to ~4GB)
pub const MAX_PAGES: u32 = 1_048_576;

/// Magic string for CoW + dual-superblock format (v2)
pub const COW_MAGIC: &[u8; 16] = b"KKDB COW v2\0\0\0\0\0";

const COW_SUPERBLOCK_SIZE: usize = 68;

const FNV32_OFFSET_BASIS: u32 = 0x811C_9DC5;
const FNV32_PRIME: u32 = 16_777_619;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuperblockSlot {
    A,
    B,
}

impl SuperblockSlot {
    #[inline]
    fn page_num(self) -> u32 {
        match self {
            SuperblockSlot::A => 1,
            SuperblockSlot::B => 2,
        }
    }

    #[inline]
    fn inactive(self) -> SuperblockSlot {
        match self {
            SuperblockSlot::A => SuperblockSlot::B,
            SuperblockSlot::B => SuperblockSlot::A,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerFormat {
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerFailpoint {
    AfterDataPagesWrite,
    AfterDataPagesSync,
    AfterSuperblockWrite,
    AfterSuperblockSync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerFailAction {
    Error,
    AbortProcess,
}

/// A page of data
#[derive(Clone)]
pub struct Page {
    pub data: [u8; PAGE_SIZE],
    pub dirty: bool,
    /// True if original content has already been saved in the current transaction snapshot.
    /// Avoids a HashMap lookup on every subsequent write to the same page.
    pub snapshotted: bool,
}

impl Page {
    pub fn new() -> Self {
        Page {
            data: [0u8; PAGE_SIZE],
            dirty: false,
            snapshotted: false,
        }
    }
}

/// Database file header (stored in first 100 bytes of page 1)
#[derive(Debug, Clone)]
pub struct DbHeader {
    pub page_size: u16,
    pub total_pages: u32,
    pub first_freelist_page: u32,
    pub freelist_count: u32,
    pub schema_version: u32,
}

/// CoW superblock used by format v2 (stored on page 1 and page 2).
#[derive(Debug, Clone)]
pub struct SuperblockV2 {
    pub format_version: u16,
    pub page_size: u16,
    pub flags: u32,
    pub generation: u64,
    pub db_uuid: [u8; 16],
    pub schema_root: u32,
    pub free_root: u32,
    pub pending_free_root: u32,
    pub page_count: u32,
    pub checksum: u32,
}

impl SuperblockV2 {
    pub fn new(db_uuid: [u8; 16]) -> Self {
        SuperblockV2 {
            format_version: 2,
            page_size: PAGE_SIZE as u16,
            flags: 0,
            generation: 1,
            db_uuid,
            schema_root: 3,
            free_root: 0,
            pending_free_root: 0,
            page_count: 2,
            checksum: 0,
        }
    }

    pub fn serialize(&self, buf: &mut [u8]) -> Result<()> {
        if buf.len() < COW_SUPERBLOCK_SIZE {
            return Err(KkdbError::CorruptDatabase(
                "superblock buffer too short".into(),
            ));
        }
        buf.fill(0);
        buf[0..16].copy_from_slice(COW_MAGIC);
        buf[16..18].copy_from_slice(&self.format_version.to_le_bytes());
        buf[18..20].copy_from_slice(&self.page_size.to_le_bytes());
        buf[20..24].copy_from_slice(&self.flags.to_le_bytes());
        buf[24..32].copy_from_slice(&self.generation.to_le_bytes());
        buf[32..48].copy_from_slice(&self.db_uuid);
        buf[48..52].copy_from_slice(&self.schema_root.to_le_bytes());
        buf[52..56].copy_from_slice(&self.free_root.to_le_bytes());
        buf[56..60].copy_from_slice(&self.pending_free_root.to_le_bytes());
        buf[60..64].copy_from_slice(&self.page_count.to_le_bytes());

        let checksum = checksum32(&buf[..64]);
        buf[64..68].copy_from_slice(&checksum.to_le_bytes());
        Ok(())
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < COW_SUPERBLOCK_SIZE {
            return Err(KkdbError::CorruptDatabase("superblock too short".into()));
        }
        if &buf[0..16] != COW_MAGIC {
            return Err(KkdbError::CorruptDatabase(
                "invalid superblock magic".into(),
            ));
        }
        let format_version = u16::from_le_bytes(buf[16..18].try_into().unwrap());
        if format_version != 2 {
            return Err(KkdbError::CorruptDatabase(format!(
                "unsupported superblock version: {}",
                format_version
            )));
        }
        let page_size = u16::from_le_bytes(buf[18..20].try_into().unwrap());
        if page_size as usize != PAGE_SIZE {
            return Err(KkdbError::CorruptDatabase(format!(
                "unsupported page size: {}",
                page_size
            )));
        }

        let stored_checksum = u32::from_le_bytes(buf[64..68].try_into().unwrap());
        let computed_checksum = checksum32(&buf[..64]);
        if stored_checksum != computed_checksum {
            return Err(KkdbError::CorruptDatabase(
                "superblock checksum mismatch".into(),
            ));
        }

        Ok(SuperblockV2 {
            format_version,
            page_size,
            flags: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
            generation: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            db_uuid: buf[32..48].try_into().unwrap(),
            schema_root: u32::from_le_bytes(buf[48..52].try_into().unwrap()),
            free_root: u32::from_le_bytes(buf[52..56].try_into().unwrap()),
            pending_free_root: u32::from_le_bytes(buf[56..60].try_into().unwrap()),
            page_count: u32::from_le_bytes(buf[60..64].try_into().unwrap()),
            checksum: stored_checksum,
        })
    }
}

#[derive(Debug, Clone)]
struct CowTxnState {
    txid: u64,
    base_generation: u64,
    target_generation: u64,
    freed_root: Option<u32>,
    freed_tail: Option<u32>,
}

#[derive(Debug, Clone)]
struct CowPagerState {
    active_superblock: SuperblockV2,
    active_slot: SuperblockSlot,
    next_txid: u64,
    active_tx: Option<CowTxnState>,
}

#[inline]
fn checksum32(bytes: &[u8]) -> u32 {
    let mut h = FNV32_OFFSET_BASIS;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(FNV32_PRIME);
    }
    h
}

#[inline]
fn generate_db_uuid() -> [u8; 16] {
    let mut out = [0u8; 16];
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    out[0..8].copy_from_slice(&(nanos as u64).to_le_bytes());
    out[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    out[12..16].copy_from_slice(&((nanos >> 64) as u32).to_le_bytes());
    out
}

/// COW transaction snapshot — only stores pages that have actually been modified.
/// begin_transaction is O(1); each modified page is saved once on first write.
struct TxnSnapshot {
    header: DbHeader,
    /// Superblock and slot as-of BEGIN, needed to fully restore cow_state on rollback.
    cow_superblock: SuperblockV2,
    cow_slot: SuperblockSlot,
    /// page_num → original 4 KB content before the first write in this transaction.
    original_pages: std::collections::HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// Total page count at BEGIN, used to truncate pages/loaded on rollback.
    original_total_pages: u32,
}

/// Watermark record for a named savepoint inside a transaction.
/// Shares the transaction's `original_pages` HashMap; only stores the set of
/// page numbers that were already snapshotted when the savepoint was created.
struct SavepointMarker {
    name: String,
    header: DbHeader,
    cow_superblock: SuperblockV2,
    cow_slot: SuperblockSlot,
    original_total_pages: u32,
    /// Pages already snapshotted at savepoint-creation time (the "watermark").
    /// Rolling back to this savepoint restores only pages added *after* this set.
    watermark_keys: std::collections::HashSet<u32>,
}

/// Pager manages reading/writing pages to/from disk
/// Page numbers are 1-indexed (page 0 is invalid, like SQLite)
pub struct Pager {
    file: Option<File>,
    pub header: DbHeader,
    /// Pages stored at index (page_num - 1) for O(1) direct access
    pages: Vec<Page>,
    /// Track which pages have been loaded from disk (file-based only)
    loaded: Vec<bool>,
    pub is_memory: bool,
    format: PagerFormat,
    cow_state: Option<CowPagerState>,
    failpoint: Option<PagerFailpoint>,
    fail_action: PagerFailAction,
    /// COW transaction snapshot — O(1) to begin, O(dirty_pages) to rollback.
    txn_snapshot: Option<TxnSnapshot>,
    /// Named savepoint stack (watermark-based, shared original_pages with txn_snapshot).
    savepoint_stack: Vec<SavepointMarker>,
    /// When true, fsync calls inside commit/flush are skipped.
    /// Use only for bulk-import scenarios where crash-recovery is acceptable via replay.
    bulk_mode: bool,
}

impl Pager {
    #[inline]
    fn init_leaf_table_page(page: &mut [u8], header_offset: usize) {
        page[header_offset] = 0x0D; // leaf table b-tree page type
        page[header_offset + 1..header_offset + 3].copy_from_slice(&0u16.to_le_bytes()); // cell count
        page[header_offset + 3..header_offset + 5]
            .copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes()); // cell content area start
        page[header_offset + 5] = 0; // fragmented free bytes
    }

    fn read_superblock_v2(file: &mut File, page_num: u32) -> Result<SuperblockV2> {
        let mut page = [0u8; PAGE_SIZE];
        let offset = (page_num as u64 - 1) * PAGE_SIZE as u64;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut page).map_err(|e| {
            if e.kind() == ErrorKind::UnexpectedEof {
                KkdbError::CorruptDatabase(format!("short read for superblock {}", page_num))
            } else {
                KkdbError::Io(e)
            }
        })?;
        SuperblockV2::deserialize(&page[..COW_SUPERBLOCK_SIZE])
    }

    #[cfg(test)]
    fn read_active_superblock_v2_with_slot(
        file: &mut File,
    ) -> Result<(SuperblockV2, SuperblockSlot)> {
        let left = Self::read_superblock_v2(file, 1);
        let right = Self::read_superblock_v2(file, 2);
        match (left, right) {
            (Ok(a), Ok(b)) => {
                if a.db_uuid != b.db_uuid {
                    return Err(KkdbError::CorruptDatabase(
                        "superblock UUID mismatch across slots".into(),
                    ));
                }
                if b.generation > a.generation {
                    Ok((b, SuperblockSlot::B))
                } else {
                    Ok((a, SuperblockSlot::A))
                }
            }
            (Ok(a), Err(_)) => Ok((a, SuperblockSlot::A)),
            (Err(_), Ok(b)) => Ok((b, SuperblockSlot::B)),
            (Err(_), Err(_)) => Err(KkdbError::CorruptDatabase(
                "both superblock slots are invalid".into(),
            )),
        }
    }

    #[inline]
    fn superblock_fits_file(sb: &SuperblockV2, file_len: u64) -> bool {
        if sb.page_count < 3 {
            return false;
        }
        let expected_len = (sb.page_count as u64) * PAGE_SIZE as u64;
        file_len >= expected_len
    }

    fn choose_openable_superblock_v2(
        left: Result<SuperblockV2>,
        right: Result<SuperblockV2>,
        file_len: u64,
    ) -> Result<(SuperblockV2, SuperblockSlot)> {
        match (left, right) {
            (Ok(a), Ok(b)) => {
                if a.db_uuid != b.db_uuid {
                    return Err(KkdbError::CorruptDatabase(
                        "superblock UUID mismatch across slots".into(),
                    ));
                }
                let a_ok = Self::superblock_fits_file(&a, file_len);
                let b_ok = Self::superblock_fits_file(&b, file_len);
                match (a_ok, b_ok) {
                    (true, true) => {
                        if b.generation > a.generation {
                            Ok((b, SuperblockSlot::B))
                        } else {
                            Ok((a, SuperblockSlot::A))
                        }
                    }
                    (true, false) => Ok((a, SuperblockSlot::A)),
                    (false, true) => Ok((b, SuperblockSlot::B)),
                    (false, false) => Err(KkdbError::CorruptDatabase(format!(
                        "no openable superblock slot for file len {}",
                        file_len
                    ))),
                }
            }
            (Ok(a), Err(_)) => {
                if Self::superblock_fits_file(&a, file_len) {
                    Ok((a, SuperblockSlot::A))
                } else {
                    Err(KkdbError::CorruptDatabase(format!(
                        "superblock A does not fit file len {}",
                        file_len
                    )))
                }
            }
            (Err(_), Ok(b)) => {
                if Self::superblock_fits_file(&b, file_len) {
                    Ok((b, SuperblockSlot::B))
                } else {
                    Err(KkdbError::CorruptDatabase(format!(
                        "superblock B does not fit file len {}",
                        file_len
                    )))
                }
            }
            (Err(_), Err(_)) => Err(KkdbError::CorruptDatabase(
                "both superblock slots are invalid".into(),
            )),
        }
    }

    fn write_superblock_v2(file: &mut File, slot: SuperblockSlot, sb: &SuperblockV2) -> Result<()> {
        let mut page = [0u8; PAGE_SIZE];
        sb.serialize(&mut page)?;
        let offset = (slot.page_num() as u64 - 1) * PAGE_SIZE as u64;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&page)?;
        Ok(())
    }

    fn write_superblock_v2_memory(
        pages: &mut [Page],
        slot: SuperblockSlot,
        sb: &SuperblockV2,
    ) -> Result<()> {
        let idx = (slot.page_num() - 1) as usize;
        if idx >= pages.len() {
            return Err(KkdbError::CorruptDatabase(format!(
                "missing superblock page for slot {:?}",
                slot
            )));
        }
        sb.serialize(&mut pages[idx].data)?;
        pages[idx].dirty = false;
        Ok(())
    }

    #[inline]
    fn sync_file_data(file: &mut File) -> Result<()> {
        file.sync_data()?;
        Ok(())
    }

    #[cfg(unix)]
    fn sync_parent_dir(path: &Path) -> Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let dir_file = OpenOptions::new().read(true).open(dir)?;
        dir_file.sync_all()?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn sync_parent_dir(_path: &Path) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn sync_db_file(&mut self) -> Result<()> {
        if let Some(ref mut file) = self.file {
            Self::sync_file_data(file)?;
        }
        Ok(())
    }

    #[inline]
    fn maybe_failpoint(&self, failpoint: PagerFailpoint) -> Result<()> {
        if self.failpoint == Some(failpoint) {
            match self.fail_action {
                PagerFailAction::Error => {
                    return Err(KkdbError::RuntimeError(format!(
                        "injected pager failpoint: {:?}",
                        failpoint
                    )));
                }
                PagerFailAction::AbortProcess => std::process::abort(),
            }
        }
        Ok(())
    }

    #[inline]
    fn write_v2_superblock(&mut self, slot: SuperblockSlot, sb: &SuperblockV2) -> Result<()> {
        if let Some(ref mut file) = self.file {
            Self::write_superblock_v2(file, slot, sb)
        } else {
            Self::write_superblock_v2_memory(&mut self.pages, slot, sb)
        }
    }

    pub fn open_cow_v2<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < (PAGE_SIZE as u64) * 2 {
            return Err(KkdbError::CorruptDatabase(
                "format v2 file is too small".into(),
            ));
        }

        let mut magic = [0u8; 16];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut magic)?;
        if &magic != COW_MAGIC {
            return Err(KkdbError::CorruptDatabase(
                "database is not format v2".into(),
            ));
        }

        let left = Self::read_superblock_v2(&mut file, 1);
        let right = Self::read_superblock_v2(&mut file, 2);
        let (active_superblock, active_slot) =
            Self::choose_openable_superblock_v2(left, right, file_len)?;
        let next_txid = active_superblock.generation.saturating_add(1);

        let total = active_superblock.page_count as usize;
        let mut pages = Vec::with_capacity(total);
        for _ in 0..total {
            pages.push(Page::new());
        }
        let loaded = vec![false; total];
        Ok(Pager {
            file: Some(file),
            header: DbHeader {
                page_size: PAGE_SIZE as u16,
                total_pages: active_superblock.page_count,
                first_freelist_page: 0,
                freelist_count: 0,
                schema_version: 0,
            },
            pages,
            loaded,
            is_memory: false,
            format: PagerFormat::V2,
            cow_state: Some(CowPagerState {
                active_superblock,
                active_slot,
                next_txid,
                active_tx: None,
            }),
            failpoint: None,
            fail_action: PagerFailAction::Error,
            txn_snapshot: None,
            savepoint_stack: Vec::new(),
            bulk_mode: false,
        })
    }

    pub fn create_cow_v2<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;

        let mut sb_a = SuperblockV2::new(generate_db_uuid());
        sb_a.generation = 1;
        sb_a.page_count = 3;

        let mut sb_b = sb_a.clone();
        sb_b.generation = 0;

        let mut schema_page = [0u8; PAGE_SIZE];
        Self::init_leaf_table_page(&mut schema_page, 0);

        Self::write_superblock_v2(&mut file, SuperblockSlot::A, &sb_a)?;
        Self::write_superblock_v2(&mut file, SuperblockSlot::B, &sb_b)?;
        file.seek(SeekFrom::Start((3u64 - 1) * PAGE_SIZE as u64))?;
        file.write_all(&schema_page)?;
        Self::sync_file_data(&mut file)?;
        Self::sync_parent_dir(path)?;
        drop(file);

        Self::open_cow_v2(path)
    }

    /// Load a single page from disk if not yet loaded (lazy, on-demand).
    /// Replaces the old load_all_pages_for_snapshot which was O(N) at BEGIN.
    #[allow(dead_code)]
    #[inline]
    fn ensure_page_loaded(&mut self, page_num: u32) -> Result<()> {
        let idx = (page_num - 1) as usize;
        if !self.loaded[idx] {
            Self::load_page_from_disk(&mut self.file, page_num, &mut self.pages[idx])?;
            self.loaded[idx] = true;
        }
        Ok(())
    }

    fn write_v2_data_pages(&mut self) -> Result<()> {
        if self.pages.len() >= 2 && (self.pages[0].dirty || self.pages[1].dirty) {
            return Err(KkdbError::RuntimeError(
                "format v2 reserves page 1/2 for superblocks".into(),
            ));
        }

        for (idx, page) in self.pages.iter_mut().enumerate() {
            if page.dirty {
                let page_num = (idx + 1) as u32;
                if page_num <= 2 {
                    return Err(KkdbError::RuntimeError(
                        "format v2 reserves page 1/2 for superblocks".into(),
                    ));
                }
                if let Some(ref mut file) = self.file {
                    let offset = (idx as u64) * PAGE_SIZE as u64;
                    file.seek(SeekFrom::Start(offset))?;
                    file.write_all(&page.data)?;
                }
                page.dirty = false;
            }
        }
        Ok(())
    }

    fn flush_v2_data_pages(&mut self) -> Result<()> {
        self.write_v2_data_pages()?;
        self.sync_db_file()?;
        Ok(())
    }

    fn commit_transaction_v2(&mut self) -> Result<()> {
        let (inactive_slot, new_superblock) = {
            let state = self
                .cow_state
                .as_ref()
                .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
            let tx = state
                .active_tx
                .as_ref()
                .ok_or_else(|| KkdbError::RuntimeError("transaction not active".into()))?;
            if tx.base_generation != state.active_superblock.generation {
                return Err(KkdbError::RuntimeError(format!(
                    "stale v2 transaction state for txid {}",
                    tx.txid
                )));
            }
            let mut sb = state.active_superblock.clone();
            sb.generation = tx.target_generation;
            sb.page_count = self.header.total_pages;
            (state.active_slot.inactive(), sb)
        };

        // Step 1-2: write dirty data pages and fsync database file.
        self.write_v2_data_pages()?;
        self.maybe_failpoint(PagerFailpoint::AfterDataPagesWrite)?;
        if !self.bulk_mode {
            self.sync_db_file()?;
        }
        self.maybe_failpoint(PagerFailpoint::AfterDataPagesSync)?;

        // Step 3-4: write inactive superblock and fsync database file.
        self.write_v2_superblock(inactive_slot, &new_superblock)?;
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockWrite)?;
        if !self.bulk_mode {
            self.sync_db_file()?;
        }
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockSync)?;

        let state = self
            .cow_state
            .as_mut()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        state.active_superblock = new_superblock;
        state.active_slot = inactive_slot;
        
        let mut pending_update_tail = None;
        if let Some(state) = self.cow_state.as_mut() {
            if let Some(tx) = &state.active_tx {
                if let Some(freed_root) = tx.freed_root {
                    if let Some(freed_tail) = tx.freed_tail {
                        let old_pending = state.active_superblock.pending_free_root;
                        pending_update_tail = Some((freed_tail, old_pending));
                        state.active_superblock.pending_free_root = freed_root;
                    }
                }
            }
        }

        if let Some((freed_tail, old_pending)) = pending_update_tail {
            let tail_page = self.get_page_mut(freed_tail)?;
            tail_page.data[0..4].copy_from_slice(&old_pending.to_le_bytes());
        }
        
        // Trigger pool rotation on commit if free_root is empty to avoid stalls
        let state = self.cow_state.as_mut().unwrap();
        if state.active_superblock.free_root == 0 && state.active_superblock.pending_free_root != 0 {
             state.active_superblock.free_root = state.active_superblock.pending_free_root;
             state.active_superblock.pending_free_root = 0;
        }

        state.active_tx = None;
        // Reset snapshotted flags on all pages that were COW-tracked this transaction.
        if let Some(snap) = self.txn_snapshot.take() {
            for page_num in snap.original_pages.keys() {
                if let Some(page) = self.pages.get_mut((page_num - 1) as usize) {
                    page.snapshotted = false;
                }
            }
        }
        Ok(())
    }

    fn flush_v2_autocommit(&mut self) -> Result<()> {
        let (inactive_slot, new_superblock) = {
            let state = self
                .cow_state
                .as_ref()
                .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
            let mut sb = state.active_superblock.clone();
            sb.generation = state.active_superblock.generation.saturating_add(1);
            sb.page_count = self.header.total_pages;
            (state.active_slot.inactive(), sb)
        };

        self.write_v2_data_pages()?;
        self.maybe_failpoint(PagerFailpoint::AfterDataPagesWrite)?;
        if !self.bulk_mode {
            self.sync_db_file()?;
        }
        self.maybe_failpoint(PagerFailpoint::AfterDataPagesSync)?;

        self.write_v2_superblock(inactive_slot, &new_superblock)?;
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockWrite)?;
        if !self.bulk_mode {
            self.sync_db_file()?;
        }
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockSync)?;

        let state = self
            .cow_state
            .as_mut()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        state.active_superblock = new_superblock;
        state.active_slot = inactive_slot;
        
        // Trigger pool rotation on flush if free_root is empty
        if state.active_superblock.free_root == 0 && state.active_superblock.pending_free_root != 0 {
             state.active_superblock.free_root = state.active_superblock.pending_free_root;
             state.active_superblock.pending_free_root = 0;
        }

        state.next_txid = state.next_txid.saturating_add(1);
        Ok(())
    }

    /// Open or create a database file
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let exists = path.exists();

        if !exists {
            return Self::create_cow_v2(path);
        }

        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < 16 {
            return Err(KkdbError::CorruptDatabase(
                "database file is too short".into(),
            ));
        }

        let mut magic = [0u8; 16];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut magic)?;
        if &magic == COW_MAGIC {
            drop(file);
            return Self::open_cow_v2(path);
        }

        Err(KkdbError::RuntimeError(
            "unsupported database format: only format v2 is supported".into(),
        ))
    }

    /// Create an in-memory database
    pub fn open_memory() -> Self {
        let mut sb_a = SuperblockV2::new(generate_db_uuid());
        sb_a.generation = 1;
        sb_a.page_count = 3;
        let mut sb_b = sb_a.clone();
        sb_b.generation = 0;

        let mut page1 = Page::new();
        sb_a.serialize(&mut page1.data)
            .expect("serialize in-memory superblock A");
        let mut page2 = Page::new();
        sb_b.serialize(&mut page2.data)
            .expect("serialize in-memory superblock B");

        let mut page3 = Page::new();
        Self::init_leaf_table_page(&mut page3.data, 0);

        Pager {
            file: None,
            header: DbHeader {
                page_size: PAGE_SIZE as u16,
                total_pages: 3,
                first_freelist_page: 0,
                freelist_count: 0,
                schema_version: 0,
            },
            pages: vec![page1, page2, page3],
            loaded: vec![true, true, true],
            is_memory: true,
            format: PagerFormat::V2,
            cow_state: Some(CowPagerState {
                active_superblock: sb_a,
                active_slot: SuperblockSlot::A,
                next_txid: 2,
                active_tx: None,
            }),
            failpoint: None,
            fail_action: PagerFailAction::Error,
            txn_snapshot: None,
            savepoint_stack: Vec::new(),
            bulk_mode: false,
        }
    }

    /// Get a page (read from disk/cache)
    #[inline]
    pub fn get_page(&mut self, page_num: u32) -> Result<&Page> {
        if page_num == 0 || page_num > self.header.total_pages {
            return Err(KkdbError::PageOutOfRange(page_num));
        }
        let idx = (page_num - 1) as usize;
        // Load from disk on first access (file-based only)
        if !self.loaded[idx] {
            Self::load_page_from_disk(&mut self.file, page_num, &mut self.pages[idx])?;
            self.loaded[idx] = true;
        }
        Ok(&self.pages[idx])
    }

    /// Get a mutable page, triggering a COW snapshot of the original content on first write.
    #[inline]
    pub fn get_page_mut(&mut self, page_num: u32) -> Result<&mut Page> {
        if page_num == 0 || page_num > self.header.total_pages {
            return Err(KkdbError::PageOutOfRange(page_num));
        }
        let idx = (page_num - 1) as usize;
        if !self.loaded[idx] {
            Self::load_page_from_disk(&mut self.file, page_num, &mut self.pages[idx])?;
            self.loaded[idx] = true;
        }
        // COW: on first write within a transaction, save the original page content.
        // The `snapshotted` flag avoids a HashMap lookup on subsequent writes.
        if let Some(ref mut snap) = self.txn_snapshot {
            if !self.pages[idx].snapshotted {
                snap.original_pages
                    .insert(page_num, Box::new(self.pages[idx].data));
                self.pages[idx].snapshotted = true;
            }
        }
        self.pages[idx].dirty = true;
        Ok(&mut self.pages[idx])
    }

    /// Allocate a new page, prioritizing the free page pool
    pub fn allocate_page(&mut self) -> Result<u32> {
        if self.header.total_pages >= MAX_PAGES {
            return Err(KkdbError::DatabaseFull);
        }

        let mut alloc_from_free = None;
        if let Some(state) = self.cow_state.as_mut() {
            if state.active_superblock.free_root != 0 {
                alloc_from_free = Some(state.active_superblock.free_root);
            } else if state.active_superblock.pending_free_root != 0 {
                // Rotation: move pending to free
                state.active_superblock.free_root = state.active_superblock.pending_free_root;
                state.active_superblock.pending_free_root = 0;
                alloc_from_free = Some(state.active_superblock.free_root);
            }
        }

        if let Some(page_num) = alloc_from_free {
            // Read the next pointer from the freed page
            let current_free = self.get_page(page_num)?;
            let next_free = u32::from_le_bytes(current_free.data[0..4].try_into().unwrap());
            
            if let Some(state) = self.cow_state.as_mut() {
                state.active_superblock.free_root = next_free;
            }
            
            // Mark the page as dirty so it will be written out with its new content
            let page_mut = self.get_page_mut(page_num)?;
            page_mut.data.fill(0); // Zero it out to avoid leaking old data
            return Ok(page_num);
        }

        self.header.total_pages += 1;
        let page_num = self.header.total_pages;

        self.pages.push(Page::new());
        self.loaded.push(true);

        Ok(page_num)
    }

    /// Mark a page as free for the two-generation pool
    pub fn free_page(&mut self, page_num: u32) -> Result<()> {
        if page_num == 0 || page_num > self.header.total_pages {
            return Err(KkdbError::PageOutOfRange(page_num));
        }
        
        // Zero out the page and write the next pointer (which is currently 0)
        let page = self.get_page_mut(page_num)?;
        page.data.fill(0);
        page.data[0..4].copy_from_slice(&0u32.to_le_bytes()); // Next pointer = 0 (tail)
        
        // Append to the transaction's freed list
        let mut update_previous_tail = None;
        let mut update_autocommit_pending = None;
        
        if let Some(state) = self.cow_state.as_mut() {
            if let Some(tx) = state.active_tx.as_mut() {
                if let Some(tail) = tx.freed_tail {
                    update_previous_tail = Some(tail);
                } else {
                    tx.freed_root = Some(page_num);
                }
                tx.freed_tail = Some(page_num);
            } else {
                // If not in a transaction, just push directly to pending_free_root (autocommit semantic)
                let current_pending = state.active_superblock.pending_free_root;
                state.active_superblock.pending_free_root = page_num;
                update_autocommit_pending = Some(current_pending);
            }
        }
        
        if let Some(tail) = update_previous_tail {
            let tail_page = self.get_page_mut(tail)?;
            tail_page.data[0..4].copy_from_slice(&page_num.to_le_bytes());
        }
        
        if let Some(current_pending) = update_autocommit_pending {
            let page = self.get_page_mut(page_num)?;
            page.data[0..4].copy_from_slice(&current_pending.to_le_bytes());
        }
        
        Ok(())
    }

    /// Flush all dirty pages to disk
    #[inline]
    pub fn flush(&mut self) -> Result<()> {
        if self.in_transaction() {
            self.flush_v2_data_pages()
        } else {
            self.flush_v2_autocommit()
        }
    }

    /// Load a page from disk into an existing Page struct
    fn load_page_from_disk(file: &mut Option<File>, page_num: u32, page: &mut Page) -> Result<()> {
        if let Some(ref mut f) = file {
            let offset = (page_num as u64 - 1) * PAGE_SIZE as u64;
            f.seek(SeekFrom::Start(offset))?;
            f.read_exact(&mut page.data).map_err(|e| {
                if e.kind() == ErrorKind::UnexpectedEof {
                    KkdbError::CorruptDatabase(format!("short read for page {}", page_num))
                } else {
                    KkdbError::Io(e)
                }
            })?;
        }
        Ok(())
    }

    /// Get raw page data (for reading without caching issues)
    pub fn get_page_data(&mut self, page_num: u32) -> Result<[u8; PAGE_SIZE]> {
        let page = self.get_page(page_num)?;
        Ok(page.data)
    }

    /// Begin a transaction — O(1). No pages are cloned upfront.
    /// Modified pages are COW-snapshotted lazily on first write via `get_page_mut`.
    pub fn begin_transaction(&mut self) -> Result<()> {
        if self.in_transaction() {
            return Err(KkdbError::RuntimeError("transaction already active".into()));
        }
        let state = self
            .cow_state
            .as_ref()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        self.txn_snapshot = Some(TxnSnapshot {
            header: self.header.clone(),
            cow_superblock: state.active_superblock.clone(),
            cow_slot: state.active_slot,
            original_pages: std::collections::HashMap::new(),
            original_total_pages: self.header.total_pages,
        });
        let state = self.cow_state.as_mut().unwrap();
        let txid = state.next_txid;
        state.next_txid = state.next_txid.saturating_add(1);
        let base = state.active_superblock.generation;
        state.active_tx = Some(CowTxnState {
            txid,
            base_generation: base,
            target_generation: base.saturating_add(1),
            freed_root: None,
            freed_tail: None,
        });
        Ok(())
    }

    /// Return the currently active transaction ID, if any.
    pub fn active_txid(&self) -> Option<u64> {
        self.cow_state.as_ref()
            .and_then(|s| s.active_tx.as_ref().map(|tx| tx.txid))
    }

    /// Commit the current transaction: flush to disk and discard snapshot.
    pub fn commit_transaction(&mut self) -> Result<()> {
        // No active transaction -> no-op (SQLite behavior)
        if !self.in_transaction() {
            return Ok(());
        }
        self.commit_transaction_v2()
    }

    /// Rollback the current transaction: restore only modified pages from COW snapshot.
    pub fn rollback_transaction(&mut self) -> Result<()> {
        self.savepoint_stack.clear();
        if let Some(snap) = self.txn_snapshot.take() {
            // Restore header and cow_state.
            self.header = snap.header;
            if let Some(ref mut state) = self.cow_state {
                state.active_superblock = snap.cow_superblock;
                state.active_slot = snap.cow_slot;
                state.active_tx = None;
            }
            // Restore original content of each COW-snapshotted page, then clear flag.
            for (page_num, original_data) in &snap.original_pages {
                let page = &mut self.pages[(page_num - 1) as usize];
                page.data = **original_data;
                page.dirty = false;
                page.snapshotted = false;
            }
            // Truncate pages/loaded to remove any pages allocated during the transaction.
            let keep = snap.original_total_pages as usize;
            self.pages.truncate(keep);
            self.loaded.truncate(keep);
            // Note: non-snapshotted pages that existed before this transaction were never
            // written to (snapshotted=false means untouched), so no additional reset needed.
        } else {
            // No active transaction: clear active_tx as SQLite no-op behaviour.
            if let Some(state) = self.cow_state.as_mut() {
                state.active_tx = None;
            }
        }
        Ok(())
    }

    /// Create a named savepoint using the watermark approach.
    /// Records which pages are already snapshotted so rollback can undo only
    /// pages dirtied *after* this savepoint was created — no page cloning.
    pub fn savepoint(&mut self, name: &str) -> Result<()> {
        if !self.in_transaction() {
            self.begin_transaction()?;
        }
        // Remove any existing savepoint with the same name (SQLite behaviour).
        self.savepoint_stack.retain(|m| !m.name.eq_ignore_ascii_case(name));
        let state = self
            .cow_state
            .as_ref()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        let watermark_keys: std::collections::HashSet<u32> = self
            .txn_snapshot
            .as_ref()
            .map(|s| s.original_pages.keys().copied().collect())
            .unwrap_or_default();
        self.savepoint_stack.push(SavepointMarker {
            name: name.to_string(),
            header: self.header.clone(),
            cow_superblock: state.active_superblock.clone(),
            cow_slot: state.active_slot,
            original_total_pages: self.header.total_pages,
            watermark_keys,
        });
        Ok(())
    }

    /// Release (commit) a named savepoint and all savepoints after it.
    pub fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let pos = self
            .savepoint_stack
            .iter()
            .rposition(|m| m.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| KkdbError::RuntimeError(format!("savepoint '{}' not found", name)))?;
        self.savepoint_stack.truncate(pos);
        Ok(())
    }

    /// Roll back to a named savepoint, restoring only pages dirtied after it was created.
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let pos = self
            .savepoint_stack
            .iter()
            .rposition(|m| m.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| KkdbError::RuntimeError(format!("savepoint '{}' not found", name)))?;

        // Truncate to pos+1 keeping the savepoint itself, then borrow it.
        self.savepoint_stack.truncate(pos + 1);
        // marker is the last element; pull it out without removing (keep at pos).
        let marker = &self.savepoint_stack[pos];

        // Restore header and cow_state to savepoint state.
        self.header = marker.header.clone();
        if let Some(ref mut state) = self.cow_state {
            state.active_superblock = marker.cow_superblock.clone();
            state.active_slot = marker.cow_slot;
        }
        let original_total_pages = marker.original_total_pages;
        let watermark_keys = marker.watermark_keys.clone();

        // From the txn_snapshot's original_pages, restore pages that were snapshotted
        // *after* this savepoint (i.e. not in watermark_keys).
        if let Some(ref snap) = self.txn_snapshot {
            for (page_num, original_data) in &snap.original_pages {
                if !watermark_keys.contains(page_num) {
                    let page = &mut self.pages[(page_num - 1) as usize];
                    page.data = **original_data;
                    page.dirty = false;
                    page.snapshotted = false;
                    // Remove from txn_snapshot so a subsequent write COW-saves again.
                }
            }
        }
        // Remove post-savepoint entries from original_pages.
        if let Some(ref mut snap) = self.txn_snapshot {
            snap.original_pages.retain(|k, _| watermark_keys.contains(k));
            snap.header = self.header.clone();
            if let Some(state) = self.cow_state.as_ref() {
                snap.cow_superblock = state.active_superblock.clone();
                snap.cow_slot = state.active_slot;
            }
            snap.original_total_pages = original_total_pages;
        }
        // Truncate any pages allocated after the savepoint.
        self.pages.truncate(original_total_pages as usize);
        self.loaded.truncate(original_total_pages as usize);
        Ok(())
    }

    /// Enable or disable bulk-insert mode.
    /// In bulk mode the per-commit fsync is skipped — use only for idempotent bulk loads.
    pub fn set_bulk_mode(&mut self, enabled: bool) {
        self.bulk_mode = enabled;
    }

    pub fn format(&self) -> PagerFormat {
        self.format
    }

    pub fn schema_root_page(&self) -> u32 {
        self.cow_state
            .as_ref()
            .map(|s| s.active_superblock.schema_root)
            .unwrap_or(3)
    }

    pub fn set_schema_root_page(&mut self, new_root: u32) -> Result<()> {
        if new_root < 3 || new_root > self.header.total_pages {
            return Err(KkdbError::PageOutOfRange(new_root));
        }
        let state = self
            .cow_state
            .as_mut()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        state.active_superblock.schema_root = new_root;
        Ok(())
    }

    pub fn set_failpoint(&mut self, failpoint: Option<PagerFailpoint>) {
        self.failpoint = failpoint;
    }

    pub fn set_failpoint_action(&mut self, action: PagerFailAction) {
        self.fail_action = action;
    }

    /// Returns true if a transaction is currently active.
    pub fn in_transaction(&self) -> bool {
        self.cow_state
            .as_ref()
            .and_then(|s| s.active_tx.as_ref())
            .is_some()
    }
}

#[cfg(test)]
#[path = "pager_tests.rs"]
mod tests;
