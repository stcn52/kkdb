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
}

impl Page {
    pub fn new() -> Self {
        Page {
            data: [0u8; PAGE_SIZE],
            dirty: false,
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
    /// Transaction snapshot: (header, pages, loaded) saved at BEGIN
    txn_snapshot: Option<(DbHeader, Vec<Page>, Vec<bool>)>,
    /// Named savepoint stack: (name, header, pages, loaded)
    savepoint_stack: Vec<(String, DbHeader, Vec<Page>, Vec<bool>)>,
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

    fn load_all_pages_for_snapshot(&mut self) -> Result<()> {
        if self.is_memory {
            return Ok(());
        }
        for i in 0..self.header.total_pages as usize {
            if !self.loaded[i] {
                let page_num = (i + 1) as u32;
                Self::load_page_from_disk(&mut self.file, page_num, &mut self.pages[i])?;
                self.loaded[i] = true;
            }
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
        self.sync_db_file()?;
        self.maybe_failpoint(PagerFailpoint::AfterDataPagesSync)?;

        // Step 3-4: write inactive superblock and fsync database file.
        self.write_v2_superblock(inactive_slot, &new_superblock)?;
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockWrite)?;
        self.sync_db_file()?;
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockSync)?;

        let state = self
            .cow_state
            .as_mut()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        state.active_superblock = new_superblock;
        state.active_slot = inactive_slot;
        state.active_tx = None;
        self.txn_snapshot = None;
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
        self.sync_db_file()?;
        self.maybe_failpoint(PagerFailpoint::AfterDataPagesSync)?;

        self.write_v2_superblock(inactive_slot, &new_superblock)?;
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockWrite)?;
        self.sync_db_file()?;
        self.maybe_failpoint(PagerFailpoint::AfterSuperblockSync)?;

        let state = self
            .cow_state
            .as_mut()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        state.active_superblock = new_superblock;
        state.active_slot = inactive_slot;
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

    /// Get a mutable page
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
        self.pages[idx].dirty = true;
        Ok(&mut self.pages[idx])
    }

    /// Allocate a new page
    #[inline]
    pub fn allocate_page(&mut self) -> Result<u32> {
        if self.header.total_pages >= MAX_PAGES {
            return Err(KkdbError::DatabaseFull);
        }

        self.header.total_pages += 1;
        let page_num = self.header.total_pages;

        self.pages.push(Page::new());
        self.loaded.push(true);

        Ok(page_num)
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

    /// Begin a transaction by snapshotting current page state.
    /// For file-based DBs, ensures all pages are loaded before snapshot.
    pub fn begin_transaction(&mut self) -> Result<()> {
        if self.in_transaction() {
            return Err(KkdbError::RuntimeError("transaction already active".into()));
        }
        self.load_all_pages_for_snapshot()?;
        self.txn_snapshot = Some((self.header.clone(), self.pages.clone(), self.loaded.clone()));
        let state = self
            .cow_state
            .as_mut()
            .ok_or_else(|| KkdbError::Internal("missing v2 state".into()))?;
        let txid = state.next_txid;
        state.next_txid = state.next_txid.saturating_add(1);
        let base = state.active_superblock.generation;
        state.active_tx = Some(CowTxnState {
            txid,
            base_generation: base,
            target_generation: base.saturating_add(1),
        });
        Ok(())
    }

    /// Commit the current transaction: flush to disk and discard snapshot.
    pub fn commit_transaction(&mut self) -> Result<()> {
        // No active transaction -> no-op (SQLite behavior)
        if !self.in_transaction() {
            return Ok(());
        }
        self.commit_transaction_v2()
    }

    /// Rollback the current transaction: restore pages from snapshot.
    pub fn rollback_transaction(&mut self) -> Result<()> {
        self.savepoint_stack.clear();
        if let Some((header, pages, loaded)) = self.txn_snapshot.take() {
            self.header = header;
            self.pages = pages;
            self.loaded = loaded;
        }
        if let Some(state) = self.cow_state.as_mut() {
            state.active_tx = None;
        }
        // No active transaction: no-op (SQLite behavior)
        Ok(())
    }

    /// Create a named savepoint (snapshot current page state with given name).
    pub fn savepoint(&mut self, name: &str) -> Result<()> {
        if !self.in_transaction() {
            self.begin_transaction()?;
        }
        // Remove any existing savepoint with same name (SQLite behavior)
        self.savepoint_stack.retain(|(n, ..)| !n.eq_ignore_ascii_case(name));
        self.load_all_pages_for_snapshot()?;
        self.savepoint_stack.push((
            name.to_string(),
            self.header.clone(),
            self.pages.clone(),
            self.loaded.clone(),
        ));
        Ok(())
    }

    /// Release (commit) a named savepoint and all savepoints after it.
    pub fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let pos = self
            .savepoint_stack
            .iter()
            .rposition(|(n, ..)| n.eq_ignore_ascii_case(name))
            .ok_or_else(|| KkdbError::RuntimeError(format!("savepoint '{}' not found", name)))?;
        self.savepoint_stack.truncate(pos);
        Ok(())
    }

    /// Roll back to a named savepoint, discarding changes made after it.
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let pos = self
            .savepoint_stack
            .iter()
            .rposition(|(n, ..)| n.eq_ignore_ascii_case(name))
            .ok_or_else(|| KkdbError::RuntimeError(format!("savepoint '{}' not found", name)))?;
        let (_, header, pages, loaded) = self.savepoint_stack[pos].clone();
        // Keep savepoints up to and including this one
        self.savepoint_stack.truncate(pos + 1);
        self.header = header;
        self.pages = pages;
        self.loaded = loaded;
        Ok(())
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
