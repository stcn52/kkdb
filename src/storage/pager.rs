use crate::error::{KkdbError, Result};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Page size in bytes (4KB, same as SQLite default)
pub const PAGE_SIZE: usize = 4096;

/// Maximum number of pages (limits DB to ~4GB)
pub const MAX_PAGES: u32 = 1_048_576;

/// Database file header size (first 100 bytes of page 1)
pub const DB_HEADER_SIZE: usize = 100;

/// Magic string at start of database file
pub const MAGIC: &[u8; 16] = b"KKDB format 1\0\0\0";

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

impl DbHeader {
    pub fn new() -> Self {
        DbHeader {
            page_size: PAGE_SIZE as u16,
            total_pages: 1, // page 1 is always the schema table root
            first_freelist_page: 0,
            freelist_count: 0,
            schema_version: 0,
        }
    }

    pub fn serialize(&self, buf: &mut [u8]) {
        buf[0..16].copy_from_slice(MAGIC);
        buf[16..18].copy_from_slice(&self.page_size.to_le_bytes());
        buf[18..22].copy_from_slice(&self.total_pages.to_le_bytes());
        buf[22..26].copy_from_slice(&self.first_freelist_page.to_le_bytes());
        buf[26..30].copy_from_slice(&self.freelist_count.to_le_bytes());
        buf[30..34].copy_from_slice(&self.schema_version.to_le_bytes());
        // rest is reserved zeros
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < DB_HEADER_SIZE {
            return Err(KkdbError::CorruptDatabase("header too short".into()));
        }
        if &buf[0..16] != MAGIC {
            return Err(KkdbError::CorruptDatabase("invalid magic number".into()));
        }
        Ok(DbHeader {
            page_size: u16::from_le_bytes(buf[16..18].try_into().unwrap()),
            total_pages: u32::from_le_bytes(buf[18..22].try_into().unwrap()),
            first_freelist_page: u32::from_le_bytes(buf[22..26].try_into().unwrap()),
            freelist_count: u32::from_le_bytes(buf[26..30].try_into().unwrap()),
            schema_version: u32::from_le_bytes(buf[30..34].try_into().unwrap()),
        })
    }
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
    /// Transaction snapshot: (header, pages, loaded) saved at BEGIN
    txn_snapshot: Option<(DbHeader, Vec<Page>, Vec<bool>)>,
}

impl Pager {
    /// Open or create a database file
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let exists = path.exists();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let header = if exists && file.metadata()?.len() >= DB_HEADER_SIZE as u64 {
            let mut buf = [0u8; DB_HEADER_SIZE];
            file.read_exact(&mut buf)?;
            DbHeader::deserialize(&buf)?
        } else {
            let header = DbHeader::new();
            // Initialize with header and empty root page
            let mut page = Page::new();
            header.serialize(&mut page.data);
            // Initialize page 1 as a leaf node for the schema table
            // Cell count = 0, right_child = 0
            let offset = DB_HEADER_SIZE;
            page.data[offset] = 0x0D; // leaf table b-tree page type
            page.data[offset + 1..offset + 3].copy_from_slice(&0u16.to_le_bytes()); // cell count
            page.data[offset + 3..offset + 5].copy_from_slice(&((PAGE_SIZE) as u16).to_le_bytes()); // cell content area start
            page.data[offset + 5] = 0; // fragmented free bytes

            file.seek(SeekFrom::Start(0))?;
            file.write_all(&page.data)?;
            file.flush()?;
            header
        };

        let total = header.total_pages as usize;
        let mut pages = Vec::with_capacity(total);
        for _ in 0..total {
            pages.push(Page::new());
        }
        let loaded = vec![false; total];
        Ok(Pager {
            file: Some(file),
            header,
            pages,
            loaded,
            is_memory: false,
            txn_snapshot: None,
        })
    }

    /// Create an in-memory database
    pub fn open_memory() -> Self {
        let header = DbHeader::new();
        // Initialize page 1 (schema table root)
        let mut page = Page::new();
        header.serialize(&mut page.data);
        let offset = DB_HEADER_SIZE;
        page.data[offset] = 0x0D; // leaf table b-tree page
        page.data[offset + 3..offset + 5].copy_from_slice(&((PAGE_SIZE) as u16).to_le_bytes());
        page.dirty = true;
        Pager {
            file: None,
            header,
            pages: vec![page],
            loaded: vec![true],
            is_memory: true,
            txn_snapshot: None,
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
        // In-memory databases have nothing to flush
        if self.is_memory {
            return Ok(());
        }

        // Update header in page 1
        if !self.pages.is_empty() {
            self.header.serialize(&mut self.pages[0].data);
            self.pages[0].dirty = true;
        }

        if let Some(ref mut file) = self.file {
            for (idx, page) in self.pages.iter_mut().enumerate() {
                if page.dirty {
                    let offset = (idx as u64) * PAGE_SIZE as u64;
                    file.seek(SeekFrom::Start(offset))?;
                    file.write_all(&page.data)?;
                    page.dirty = false;
                }
            }
            file.flush()?;
        }

        Ok(())
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
        if self.txn_snapshot.is_some() {
            return Err(KkdbError::RuntimeError("transaction already active".into()));
        }
        // For file-based pager, load all pages so the snapshot is complete
        if !self.is_memory {
            for i in 0..self.header.total_pages as usize {
                if !self.loaded[i] {
                    let page_num = (i + 1) as u32;
                    Self::load_page_from_disk(&mut self.file, page_num, &mut self.pages[i])?;
                    self.loaded[i] = true;
                }
            }
        }
        self.txn_snapshot = Some((self.header.clone(), self.pages.clone(), self.loaded.clone()));
        Ok(())
    }

    /// Commit the current transaction: flush to disk and discard snapshot.
    pub fn commit_transaction(&mut self) -> Result<()> {
        // No active transaction -> no-op (SQLite behavior)
        if self.txn_snapshot.is_none() {
            return Ok(());
        }

        // Flush first; only clear snapshot after durable write succeeds.
        // If flush fails, keep snapshot so caller can still ROLLBACK.
        self.flush()?;
        self.txn_snapshot = None;
        Ok(())
    }

    /// Rollback the current transaction: restore pages from snapshot.
    pub fn rollback_transaction(&mut self) -> Result<()> {
        if let Some((header, pages, loaded)) = self.txn_snapshot.take() {
            self.header = header;
            self.pages = pages;
            self.loaded = loaded;
            Ok(())
        } else {
            // No active transaction — no-op (SQLite behavior)
            Ok(())
        }
    }

    /// Returns true if a transaction is currently active.
    pub fn in_transaction(&self) -> bool {
        self.txn_snapshot.is_some()
    }
}

#[cfg(test)]
#[path = "pager_tests.rs"]
mod tests;
