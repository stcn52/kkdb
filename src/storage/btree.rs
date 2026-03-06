use crate::error::{KkdbError, Result};
use crate::storage::pager::{Pager, PAGE_SIZE};
use crate::types::{deserialize_row, serialize_row, serialize_row_into, Row};

/// B-Tree page types (compatible with SQLite format concepts)
/// 0x02 = interior index b-tree page
/// 0x05 = interior table b-tree page
/// 0x0A = leaf index b-tree page
/// 0x0D = leaf table b-tree page
const LEAF_TABLE: u8 = 0x0D;
const INTERIOR_TABLE: u8 = 0x05;

/// Overflow page: [next_overflow_page: u32 (0 = end)][data: PAGE_SIZE-4 bytes]
const OVERFLOW_DATA_SIZE: usize = PAGE_SIZE - 4;

/// Maximum payload stored inline in a leaf cell.
/// If payload exceeds this, an overflow chain is written.
/// Keep well under PAGE_SIZE/2 so at least 2 cells fit per page.
const MAX_INLINE_PAYLOAD: usize = PAGE_SIZE / 2 - 32; // ≈ 2016 bytes

/// Bit-flag stored in the high bit of the on-disk `payload_size` field.
/// When set, the cell carries an overflow chain.
/// On-disk: bit31 = overflow flag; bits 0-30 = inline_payload_len.
const OVERFLOW_FLAG: u32 = 0x8000_0000;

/// B-Tree page header layout:
/// [0]      page_type (1 byte)
/// [1..3]   cell_count (2 bytes, u16 LE)
/// [3..5]   cell_content_offset (2 bytes, u16 LE) - start of cell content area
/// [5]      fragmented_free_bytes (1 byte)
/// For interior pages:
/// [6..10]  right_child_page (4 bytes, u32 LE)
///
/// After header: cell pointer array (2 bytes per pointer, u16 LE)

const LEAF_HEADER_SIZE: usize = 6;
const INTERIOR_HEADER_SIZE: usize = 10;

/// B-Tree operations on the pager
pub struct BTree<'a> {
    pub pager: &'a mut Pager,
}

impl<'a> BTree<'a> {
    pub fn new(pager: &'a mut Pager) -> Self {
        BTree { pager }
    }

    /// Get the B-tree header offset for a page.
    #[inline]
    fn header_offset(page_num: u32) -> usize {
        let _ = page_num;
        0
    }

    /// Write page header
    fn write_page_header(&mut self, page_num: u32, header: &PageHeader) -> Result<()> {
        let off = Self::header_offset(page_num);
        let page = self.pager.get_page_mut(page_num)?;

        page.data[off] = header.page_type;
        page.data[off + 1..off + 3].copy_from_slice(&header.cell_count.to_le_bytes());
        page.data[off + 3..off + 5].copy_from_slice(&header.cell_content_offset.to_le_bytes());
        page.data[off + 5] = 0; // fragmented bytes

        if let Some(rc) = header.right_child {
            page.data[off + 6..off + 10].copy_from_slice(&rc.to_le_bytes());
        }

        Ok(())
    }

    /// Initialize a new page as a leaf table page
    pub fn init_leaf_page(&mut self, page_num: u32) -> Result<()> {
        let header = PageHeader {
            page_type: LEAF_TABLE,
            cell_count: 0,
            cell_content_offset: PAGE_SIZE as u16,
            right_child: None,
        };
        self.write_page_header(page_num, &header)
    }

    /// Initialize a new page as an interior table page
    fn init_interior_page(&mut self, page_num: u32, right_child: u32) -> Result<()> {
        let header = PageHeader {
            page_type: INTERIOR_TABLE,
            cell_count: 0,
            cell_content_offset: PAGE_SIZE as u16,
            right_child: Some(right_child),
        };
        self.write_page_header(page_num, &header)
    }

    /// Create a new table, returns the root page number
    pub fn create_table(&mut self) -> Result<u32> {
        let page_num = self.pager.allocate_page()?;
        self.init_leaf_page(page_num)?;
        Ok(page_num)
    }

    // -----------------------------------------------------------------------
    // Overflow page helpers
    // -----------------------------------------------------------------------

    /// Write `payload` into a chain of overflow pages.
    /// Returns the page number of the first overflow page.
    fn write_overflow_chain(&mut self, payload: &[u8]) -> Result<u32> {
        let mut chunks = payload.chunks(OVERFLOW_DATA_SIZE).peekable();
        let first_page = self.pager.allocate_page()?;
        let mut cur_page = first_page;

        while let Some(chunk) = chunks.next() {
            let next_page = if chunks.peek().is_some() {
                let np = self.pager.allocate_page()?;
                np
            } else {
                0 // terminal
            };

            let page = self.pager.get_page_mut(cur_page)?;
            // Write next-pointer
            page.data[0..4].copy_from_slice(&next_page.to_le_bytes());
            // Write chunk data
            page.data[4..4 + chunk.len()].copy_from_slice(chunk);
            // Zero remaining bytes in last page
            if next_page == 0 && chunk.len() < OVERFLOW_DATA_SIZE {
                for b in &mut page.data[4 + chunk.len()..PAGE_SIZE] {
                    *b = 0;
                }
            }

            if chunks.peek().is_some() {
                cur_page = next_page;
            }
        }
        Ok(first_page)
    }

    /// Read the full payload for a cell, following overflow chain if needed.
    ///
    /// `raw_payload_size` is the raw u32 read from disk (may have OVERFLOW_FLAG set).
    /// `inline_start` is the byte offset inside the page where the inline payload begins
    /// (i.e. cell_offset + 12).
    /// `page_data` is the page's data slice.
    ///
    /// Returns the complete payload bytes.
    fn read_cell_payload(
        &mut self,
        raw_payload_size: u32,
        inline_start: usize,
        page_data: &[u8],
    ) -> Result<Vec<u8>> {
        if raw_payload_size & OVERFLOW_FLAG == 0 {
            // Normal cell — payload is fully inline
            let len = raw_payload_size as usize;
            return Ok(page_data[inline_start..inline_start + len].to_vec());
        }

        // Overflow cell layout (after rowid, before overflow ptr):
        //   [inline_len: u32-without-flag (0 inline bytes stored here, flag already stripped)]
        //   [total_payload_len: u32]
        //   [overflow_first_page: u32]
        //   [inline_bytes: inline_len bytes]  (always 0 in current impl)
        let inline_len = (raw_payload_size & !OVERFLOW_FLAG) as usize;
        let total_len = u32::from_le_bytes(
            page_data[inline_start..inline_start + 4].try_into().unwrap(),
        ) as usize;
        let overflow_page = u32::from_le_bytes(
            page_data[inline_start + 4..inline_start + 8].try_into().unwrap(),
        );

        let mut result = Vec::with_capacity(total_len);
        // Copy any inline prefix
        result.extend_from_slice(&page_data[inline_start + 8..inline_start + 8 + inline_len]);

        // Follow overflow chain
        let mut cur_page = overflow_page;
        while cur_page != 0 && result.len() < total_len {
            let remaining = total_len - result.len();
            let page = self.pager.get_page(cur_page)?;
            let next_page = u32::from_le_bytes(page.data[0..4].try_into().unwrap());
            let to_copy = remaining.min(OVERFLOW_DATA_SIZE);
            result.extend_from_slice(&page.data[4..4 + to_copy]);
            cur_page = next_page;
        }
        Ok(result)
    }

    /// Free an overflow chain starting at `first_page`.
    /// Each page in the chain is returned to the pager's free pool via `pager.free_page`.
    fn free_overflow_chain(&mut self, first_page: u32) -> Result<()> {
        let mut cur = first_page;
        while cur != 0 {
            // Read next pointer before handing the page back to the pool
            let next = {
                let page = self.pager.get_page(cur)?;
                u32::from_le_bytes(page.data[0..4].try_into().unwrap())
            };
            self.pager.free_page(cur)?;
            cur = next;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Public insert API
    // -----------------------------------------------------------------------

    /// Insert a row using a reusable serialize buffer (avoids per-call allocation)
    pub fn insert_with_buf(
        &mut self,
        root_page: u32,
        rowid: i64,
        row: &Row,
        buf: &mut Vec<u8>,
    ) -> Result<u32> {
        serialize_row_into(row, buf);
        match self.insert_into_page(root_page, rowid, buf)? {
            InsertResult::Done => Ok(root_page),
            InsertResult::Split {
                divider_key,
                new_page,
            } => {
                let new_root = self.pager.allocate_page()?;
                self.init_interior_page(new_root, new_page)?;
                self.insert_interior_cell(new_root, divider_key, root_page)?;
                Ok(new_root)
            }
        }
    }

    /// Insert a row into a table B-tree, returns the (possibly new) root page
    pub fn insert(&mut self, root_page: u32, rowid: i64, row: &Row) -> Result<u32> {
        let payload = serialize_row(row);
        match self.insert_into_page(root_page, rowid, &payload)? {
            InsertResult::Done => Ok(root_page),
            InsertResult::Split {
                divider_key,
                new_page,
            } => {
                // Create new root pointing to old root (left) and new_page (right)
                let new_root = self.pager.allocate_page()?;
                self.init_interior_page(new_root, new_page)?;
                self.insert_interior_cell(new_root, divider_key, root_page)?;
                Ok(new_root)
            }
        }
    }

    /// Internal insert - returns Done or Split info to propagate upward
    fn insert_into_page(
        &mut self,
        page_num: u32,
        rowid: i64,
        payload: &[u8],
    ) -> Result<InsertResult> {
        // Single page access: read header inline to determine page type and available space
        let (page_type, cell_count, cell_content_offset, _right_child) = {
            let page = self.pager.get_page(page_num)?;
            let off = Self::header_offset(page_num);
            let pt = page.data[off];
            let cc = u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap());
            let cco = u16::from_le_bytes(page.data[off + 3..off + 5].try_into().unwrap());
            let rc = if pt == INTERIOR_TABLE {
                Some(u32::from_le_bytes(
                    page.data[off + 6..off + 10].try_into().unwrap(),
                ))
            } else {
                None
            };
            (pt, cc, cco, rc)
        };

        match page_type {
            LEAF_TABLE => {
                // Does this payload need an overflow chain?
                let needs_overflow = payload.len() > MAX_INLINE_PAYLOAD;
                // Inline cell size: normal = 12 + payload.len(); overflow = 12 + 8 (total_len+first_page)
                let cell_len = if needs_overflow { 12 + 8 } else { 12 + payload.len() };

                let hdr_offset = Self::header_offset(page_num);
                let ptr_array_end = hdr_offset + LEAF_HEADER_SIZE + (cell_count as usize) * 2;
                let available = if (cell_content_offset as usize) > ptr_array_end {
                    cell_content_offset as usize - ptr_array_end
                } else {
                    0
                };

                if cell_len + 2 <= available {
                    // Fast path: write directly to page
                    if needs_overflow {
                        // Write overflow chain first, then inline cell stub
                        let overflow_page = self.write_overflow_chain(payload)?;
                        self.insert_overflow_cell_raw(page_num, rowid, payload.len() as u32, overflow_page)?;
                    } else {
                        self.insert_cell_into_leaf_raw(page_num, rowid, payload)?;
                    }
                    Ok(InsertResult::Done)
                } else {
                    // Split path: need materialized cell_data
                    let cell_data = if needs_overflow {
                        let overflow_page = self.write_overflow_chain(payload)?;
                        Self::make_overflow_cell(rowid, payload.len() as u32, overflow_page)
                    } else {
                        Self::make_leaf_cell(rowid, payload)
                    };
                    self.split_leaf(page_num, rowid, &cell_data)
                }
            }
            INTERIOR_TABLE => {
                let child_page = self.find_child_page(page_num, rowid)?;
                match self.insert_into_page(child_page, rowid, payload)? {
                    InsertResult::Done => Ok(InsertResult::Done),
                    InsertResult::Split {
                        divider_key,
                        new_page,
                    } => {
                        // Re-read header inline (may have changed after recursive insert)
                        let available = {
                            let page = self.pager.get_page(page_num)?;
                            let off = Self::header_offset(page_num);
                            let cc =
                                u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap())
                                    as usize;
                            let cco =
                                u16::from_le_bytes(page.data[off + 3..off + 5].try_into().unwrap())
                                    as usize;
                            let ptr_end = off + INTERIOR_HEADER_SIZE + cc * 2;
                            if cco > ptr_end {
                                cco - ptr_end
                            } else {
                                0
                            }
                        };
                        if 12 + 2 <= available {
                            self.insert_interior_cell(page_num, divider_key, child_page)?;
                            self.replace_child_pointer(
                                page_num,
                                child_page,
                                divider_key,
                                new_page,
                            )?;
                            Ok(InsertResult::Done)
                        } else {
                            self.split_interior(page_num, divider_key, child_page, new_page)
                        }
                    }
                }
            }
            _ => Err(KkdbError::BTreeError(format!(
                "invalid page type: 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Make a normal leaf table cell: [payload_size:u32][rowid:i64][payload]
    fn make_leaf_cell(rowid: i64, payload: &[u8]) -> Vec<u8> {
        let mut cell = Vec::with_capacity(12 + payload.len());
        cell.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        cell.extend_from_slice(&rowid.to_le_bytes());
        cell.extend_from_slice(payload);
        cell
    }

    /// Make an overflow cell stub:
    /// [payload_size|OVERFLOW_FLAG:u32][rowid:i64][total_payload_len:u32][overflow_first_page:u32]
    fn make_overflow_cell(rowid: i64, total_payload_len: u32, overflow_first_page: u32) -> Vec<u8> {
        let mut cell = Vec::with_capacity(20);
        // payload_size field: OVERFLOW_FLAG set, inline_len = 0
        cell.extend_from_slice(&OVERFLOW_FLAG.to_le_bytes());
        cell.extend_from_slice(&rowid.to_le_bytes());
        cell.extend_from_slice(&total_payload_len.to_le_bytes());
        cell.extend_from_slice(&overflow_first_page.to_le_bytes());
        cell
    }

    /// Binary-search helper: find insertion index in a leaf's pointer array.
    /// Returns `Err` if `rowid` already exists.
    fn leaf_insert_index(page_data: &[u8], ptr_base: usize, cell_count: usize, rowid: i64) -> Result<usize> {
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_ptr = ptr_base + mid * 2;
            let mid_cell_off =
                u16::from_le_bytes(page_data[mid_ptr..mid_ptr + 2].try_into().unwrap()) as usize;
            let mid_rowid = i64::from_le_bytes(
                page_data[mid_cell_off + 4..mid_cell_off + 12].try_into().unwrap(),
            );
            if mid_rowid == rowid {
                return Err(KkdbError::ConstraintViolation(format!(
                    "UNIQUE constraint failed: rowid {rowid}"
                )));
            } else if mid_rowid < rowid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// Write a pre-built cell at `new_content_offset` inside the page and update pointer array + header.
    fn write_cell_to_leaf(
        page: &mut crate::storage::pager::Page,
        hdr_offset: usize,
        cell_data: &[u8],
        insert_idx: usize,
        cell_count: usize,
    ) {
        let hdr_size = LEAF_HEADER_SIZE;
        let ptr_base = hdr_offset + hdr_size;
        let cell_content_offset =
            u16::from_le_bytes(page.data[hdr_offset + 3..hdr_offset + 5].try_into().unwrap()) as usize;
        let new_offset = cell_content_offset - cell_data.len();

        // Write cell bytes
        page.data[new_offset..new_offset + cell_data.len()].copy_from_slice(cell_data);

        // Shift pointers right
        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(page.data[src..src + 2].try_into().unwrap());
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        page.data[ptr_base + insert_idx * 2..ptr_base + insert_idx * 2 + 2]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());

        // Update header
        page.data[hdr_offset + 1..hdr_offset + 3]
            .copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[hdr_offset + 3..hdr_offset + 5]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());
    }

    /// Insert a normal (non-overflow) cell into a leaf page directly (no intermediate Vec).
    fn insert_cell_into_leaf_raw(
        &mut self,
        page_num: u32,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        let hdr_offset = Self::header_offset(page_num);
        let cell_len = 12 + payload.len();

        let page = self.pager.get_page_mut(page_num)?;
        let off = hdr_offset;
        let cell_count =
            u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize;
        let ptr_base = hdr_offset + LEAF_HEADER_SIZE;

        let insert_idx = Self::leaf_insert_index(&page.data, ptr_base, cell_count, rowid)?;

        // Build cell inline (avoid extra allocation for small payloads)
        let mut cell = [0u8; 12];
        cell[0..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        cell[4..12].copy_from_slice(&rowid.to_le_bytes());

        let cell_content_offset =
            u16::from_le_bytes(page.data[off + 3..off + 5].try_into().unwrap()) as usize;
        let new_offset = cell_content_offset - cell_len;
        page.data[new_offset..new_offset + 12].copy_from_slice(&cell);
        page.data[new_offset + 12..new_offset + cell_len].copy_from_slice(payload);

        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(page.data[src..src + 2].try_into().unwrap());
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        page.data[ptr_base + insert_idx * 2..ptr_base + insert_idx * 2 + 2]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());

        page.data[off + 1..off + 3].copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[off + 3..off + 5].copy_from_slice(&(new_offset as u16).to_le_bytes());

        Ok(())
    }

    /// Insert an overflow-cell stub into a leaf page directly.
    /// Inline portion: [OVERFLOW_FLAG | 0 : u32][rowid: i64][total_len: u32][first_overflow_page: u32] = 20 bytes
    fn insert_overflow_cell_raw(
        &mut self,
        page_num: u32,
        rowid: i64,
        total_payload_len: u32,
        overflow_first_page: u32,
    ) -> Result<()> {
        let hdr_offset = Self::header_offset(page_num);
        let cell_len = 20usize; // 4 + 8 + 4 + 4

        let page = self.pager.get_page_mut(page_num)?;
        let off = hdr_offset;
        let cell_count =
            u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize;
        let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
        let insert_idx = Self::leaf_insert_index(&page.data, ptr_base, cell_count, rowid)?;

        let cell_content_offset =
            u16::from_le_bytes(page.data[off + 3..off + 5].try_into().unwrap()) as usize;
        let new_offset = cell_content_offset - cell_len;

        page.data[new_offset..new_offset + 4].copy_from_slice(&OVERFLOW_FLAG.to_le_bytes());
        page.data[new_offset + 4..new_offset + 12].copy_from_slice(&rowid.to_le_bytes());
        page.data[new_offset + 12..new_offset + 16].copy_from_slice(&total_payload_len.to_le_bytes());
        page.data[new_offset + 16..new_offset + 20].copy_from_slice(&overflow_first_page.to_le_bytes());

        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(page.data[src..src + 2].try_into().unwrap());
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        page.data[ptr_base + insert_idx * 2..ptr_base + insert_idx * 2 + 2]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());

        page.data[off + 1..off + 3].copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[off + 3..off + 5].copy_from_slice(&(new_offset as u16).to_le_bytes());

        Ok(())
    }

    /// Insert a cell into a leaf page (assumes it fits) — takes pre-built cell_data
    fn insert_cell_into_leaf(&mut self, page_num: u32, rowid: i64, cell_data: &[u8]) -> Result<()> {
        let hdr_offset = Self::header_offset(page_num);
        let hdr_size = LEAF_HEADER_SIZE;

        // Single page access: read header, binary search, write cell, shift pointers, update header
        let page = self.pager.get_page_mut(page_num)?;
        let off = hdr_offset;
        let cell_count =
            u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize;
        let cell_content_offset =
            u16::from_le_bytes(page.data[off + 3..off + 5].try_into().unwrap()) as usize;

        // Binary search for insertion position (inline — avoids per-iteration page lookup)
        let ptr_base = hdr_offset + hdr_size;
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_ptr = ptr_base + mid * 2;
            let mid_cell_off =
                u16::from_le_bytes(page.data[mid_ptr..mid_ptr + 2].try_into().unwrap()) as usize;
            let mid_rowid = i64::from_le_bytes(
                page.data[mid_cell_off + 4..mid_cell_off + 12]
                    .try_into()
                    .unwrap(),
            );
            if mid_rowid == rowid {
                return Err(KkdbError::ConstraintViolation(format!(
                    "UNIQUE constraint failed: rowid {}",
                    rowid
                )));
            } else if mid_rowid < rowid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let insert_idx = lo;

        // Write cell content at the end of the content area
        let new_content_offset = cell_content_offset - cell_data.len();
        page.data[new_content_offset..new_content_offset + cell_data.len()]
            .copy_from_slice(cell_data);

        // Shift cell pointers to make room
        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(page.data[src..src + 2].try_into().unwrap());
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        // Write new pointer
        let ptr_offset = ptr_base + insert_idx * 2;
        page.data[ptr_offset..ptr_offset + 2]
            .copy_from_slice(&(new_content_offset as u16).to_le_bytes());

        // Update header in-place
        page.data[off + 1..off + 3].copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[off + 3..off + 5].copy_from_slice(&(new_content_offset as u16).to_le_bytes());

        Ok(())
    }

    /// Split a leaf page and insert a cell — returns Split info to propagate upward
    fn split_leaf(&mut self, page_num: u32, rowid: i64, cell_data: &[u8]) -> Result<InsertResult> {
        // Collect all raw cell bytes at once (may include overflow stubs)
        let mut cells: Vec<(i64, Vec<u8>)> = {
            let page = self.pager.get_page(page_num)?;
            let data = &page.data;
            let hdr_offset = Self::header_offset(page_num);
            let cell_count =
                u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap()) as usize;
            let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
            let mut v = Vec::with_capacity(cell_count + 1);
            for i in 0..cell_count {
                let ptr_off = ptr_base + i * 2;
                let cell_off =
                    u16::from_le_bytes(data[ptr_off..ptr_off + 2].try_into().unwrap()) as usize;
                let raw_size = u32::from_le_bytes(data[cell_off..cell_off + 4].try_into().unwrap());
                let rid = i64::from_le_bytes(data[cell_off + 4..cell_off + 12].try_into().unwrap());
                // Cell length on the page (stub only for overflow cells)
                let cell_len = if raw_size & OVERFLOW_FLAG != 0 {
                    20 // overflow stub: 4+8+4+4
                } else {
                    12 + (raw_size as usize)
                };
                v.push((rid, data[cell_off..cell_off + cell_len].to_vec()));
            }
            v
        };

        let insert_pos = cells
            .iter()
            .position(|(rid, _)| rowid < *rid)
            .unwrap_or(cells.len());
        cells.insert(insert_pos, (rowid, cell_data.to_vec()));

        let mut mid = cells.len() / 2;
        if insert_pos == cells.len() - 1 {
            mid = cells.len() - 1; // Right-Edge Append Optimization
        }

        let divider_key = cells[mid].0;

        self.init_leaf_page(page_num)?;
        for (rid, cell) in &cells[..mid] {
            self.insert_cell_into_leaf(page_num, *rid, cell)?;
        }

        let right_page_num = self.pager.allocate_page()?;
        self.init_leaf_page(right_page_num)?;
        for (rid, cell) in &cells[mid..] {
            self.insert_cell_into_leaf(right_page_num, *rid, cell)?;
        }

        Ok(InsertResult::Split {
            divider_key,
            new_page: right_page_num,
        })
    }

    /// Split an interior page — returns Split info to propagate upward
    fn split_interior(
        &mut self,
        page_num: u32,
        new_key: i64,
        left_child: u32,
        right_child_of_new: u32,
    ) -> Result<InsertResult> {
        // Single page access: read header + collect all cells
        let (mut cells, old_right_child) = {
            let page = self.pager.get_page(page_num)?;
            let data = &page.data;
            let hdr_offset = Self::header_offset(page_num);
            let cell_count =
                u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap())
                    as usize;
            let rc = u32::from_le_bytes(data[hdr_offset + 6..hdr_offset + 10].try_into().unwrap());
            let mut v = Vec::with_capacity(cell_count + 1);
            for i in 0..cell_count {
                let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + i * 2;
                let cell_offset =
                    u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap())
                        as usize;
                let child =
                    u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
                let key =
                    i64::from_le_bytes(data[cell_offset + 4..cell_offset + 12].try_into().unwrap());
                v.push((child, key));
            }
            (v, rc)
        };

        // Insert new cell in sorted position
        // The new cell: left_child points to left_child, key = new_key
        // After this cell, the next cell's left_child (or right_child) = right_child_of_new
        let insert_pos = cells
            .iter()
            .position(|(_, k)| new_key < *k)
            .unwrap_or(cells.len());
        cells.insert(insert_pos, (left_child, new_key));

        // Fix child pointers: the cell at insert_pos+1 should have left_child = right_child_of_new
        // Or if insert_pos is last, the page's right_child = right_child_of_new
        let final_right_child;
        if insert_pos + 1 < cells.len() {
            cells[insert_pos + 1].0 = right_child_of_new;
            final_right_child = old_right_child;
        } else {
            final_right_child = right_child_of_new;
        }

        let mut mid = cells.len() / 2;
        // Right-Edge Append Optimization for Interior Nodes
        if insert_pos == cells.len() - 1 {
            mid = cells.len() - 1;
        }
        
        // Split at midpoint: promote middle key
        let promoted_key = cells[mid].1;

        // Left page (reuse current page): cells[0..mid], right_child = cells[mid].0
        let left_right_child = cells[mid].0;
        self.init_interior_page(page_num, left_right_child)?;
        for i in 0..mid {
            self.insert_interior_cell(page_num, cells[i].1, cells[i].0)?;
        }

        // Right page (new): cells[mid+1..], right_child = final_right_child
        let new_page = self.pager.allocate_page()?;
        self.init_interior_page(new_page, final_right_child)?;
        for i in (mid + 1)..cells.len() {
            self.insert_interior_cell(new_page, cells[i].1, cells[i].0)?;
        }

        Ok(InsertResult::Split {
            divider_key: promoted_key,
            new_page,
        })
    }

    /// After inserting a divider cell, update the child pointer:
    /// The cell with key = divider_key has left_child = old_child_page.
    /// The right side of that divider should point to new_page.
    /// This means: if there's a cell after divider_key, its left_child = new_page.
    /// Otherwise, the page's right_child = new_page.
    fn replace_child_pointer(
        &mut self,
        page_num: u32,
        _old_child: u32,
        divider_key: i64,
        new_page: u32,
    ) -> Result<()> {
        let hdr_offset = Self::header_offset(page_num);
        // Single page access: read header, find divider, update pointer
        let page = self.pager.get_page_mut(page_num)?;
        let off = hdr_offset;
        let cell_count =
            u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize;
        let ptr_base = hdr_offset + INTERIOR_HEADER_SIZE;

        // Find the cell with key == divider_key
        let mut divider_idx = None;
        for i in 0..cell_count {
            let p = ptr_base + i * 2;
            let cell_off = u16::from_le_bytes(page.data[p..p + 2].try_into().unwrap()) as usize;
            let key =
                i64::from_le_bytes(page.data[cell_off + 4..cell_off + 12].try_into().unwrap());
            if key == divider_key {
                divider_idx = Some(i);
                break;
            }
        }

        let div_i = divider_idx
            .ok_or_else(|| KkdbError::BTreeError("divider key not found after insert".into()))?;

        if div_i + 1 < cell_count {
            // Update left_child of the NEXT cell to new_page
            let next_ptr_offset = ptr_base + (div_i + 1) * 2;
            let next_cell_offset = u16::from_le_bytes(
                page.data[next_ptr_offset..next_ptr_offset + 2]
                    .try_into()
                    .unwrap(),
            ) as usize;
            page.data[next_cell_offset..next_cell_offset + 4]
                .copy_from_slice(&new_page.to_le_bytes());
        } else {
            // divider is the last cell — update right_child in header
            page.data[off + 6..off + 10].copy_from_slice(&new_page.to_le_bytes());
        }

        Ok(())
    }

    /// Insert a cell into an interior page (sorted by key)
    fn insert_interior_cell(&mut self, page_num: u32, key: i64, left_child: u32) -> Result<()> {
        let hdr_offset = Self::header_offset(page_num);

        // Interior cell: [left_child_page: u32][key: i64] = 12 bytes
        let mut cell_data = [0u8; 12];
        cell_data[0..4].copy_from_slice(&left_child.to_le_bytes());
        cell_data[4..12].copy_from_slice(&key.to_le_bytes());

        // Single page access: read header, binary search, write cell, shift pointers, update header
        let page = self.pager.get_page_mut(page_num)?;
        let off = hdr_offset;
        let cell_count =
            u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize;
        let cell_content_offset =
            u16::from_le_bytes(page.data[off + 3..off + 5].try_into().unwrap()) as usize;

        // Binary search for insertion position (inline — avoids per-iteration page lookup)
        let ptr_base = hdr_offset + INTERIOR_HEADER_SIZE;
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_ptr = ptr_base + mid * 2;
            let mid_cell_off =
                u16::from_le_bytes(page.data[mid_ptr..mid_ptr + 2].try_into().unwrap()) as usize;
            let mid_key = i64::from_le_bytes(
                page.data[mid_cell_off + 4..mid_cell_off + 12]
                    .try_into()
                    .unwrap(),
            );
            if key < mid_key {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let insert_idx = lo;

        // Write cell content
        let new_content_offset = cell_content_offset - cell_data.len();
        page.data[new_content_offset..new_content_offset + cell_data.len()]
            .copy_from_slice(&cell_data);

        // Shift cell pointers to make room at insert_idx
        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(page.data[src..src + 2].try_into().unwrap());
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        // Write new pointer at insert_idx
        let ptr_offset = ptr_base + insert_idx * 2;
        page.data[ptr_offset..ptr_offset + 2]
            .copy_from_slice(&(new_content_offset as u16).to_le_bytes());

        // Update header in-place
        page.data[off + 1..off + 3].copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[off + 3..off + 5].copy_from_slice(&(new_content_offset as u16).to_le_bytes());

        Ok(())
    }

    /// Find the child page for a given rowid in an interior page (binary search)
    fn find_child_page(&mut self, page_num: u32, rowid: i64) -> Result<u32> {
        let page = self.pager.get_page(page_num)?;
        let data = &page.data;
        let hdr_offset = Self::header_offset(page_num);

        let cell_count =
            u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap()) as usize;
        let right_child =
            u32::from_le_bytes(data[hdr_offset + 6..hdr_offset + 10].try_into().unwrap());

        if cell_count == 0 {
            return Ok(right_child);
        }

        // Binary search: find first cell where key > rowid
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + mid * 2;
            let cell_offset =
                u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap()) as usize;
            let key =
                i64::from_le_bytes(data[cell_offset + 4..cell_offset + 12].try_into().unwrap());

            if rowid < key {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }

        // lo = first cell where key > rowid
        if lo < cell_count {
            let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + lo * 2;
            let cell_offset =
                u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap()) as usize;
            let child = u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
            Ok(child)
        } else {
            Ok(right_child)
        }
    }

    /// Scan all rows in a table (full table scan)
    pub fn scan_all(&mut self, root_page: u32) -> Result<Vec<(i64, Row)>> {
        // Pre-allocate with a hint from root page cell count
        let page = self.pager.get_page(root_page)?;
        let hdr_offset = Self::header_offset(root_page);
        let page_type = page.data[hdr_offset];
        let cell_count = u16::from_le_bytes(
            page.data[hdr_offset + 1..hdr_offset + 3]
                .try_into()
                .unwrap(),
        ) as usize;
        let capacity = if page_type == LEAF_TABLE {
            cell_count
        } else {
            cell_count * 200
        };
        let mut results = Vec::with_capacity(capacity);
        self.scan_page(root_page, &mut results)?;
        Ok(results)
    }

    /// Scan all rows without rowids (for SELECT — avoids tuple overhead)
    pub fn scan_rows(&mut self, root_page: u32) -> Result<Vec<Row>> {
        let page = self.pager.get_page(root_page)?;
        let hdr_offset = Self::header_offset(root_page);
        let page_type = page.data[hdr_offset];
        let cell_count = u16::from_le_bytes(
            page.data[hdr_offset + 1..hdr_offset + 3]
                .try_into()
                .unwrap(),
        ) as usize;
        let capacity = if page_type == LEAF_TABLE {
            cell_count
        } else {
            cell_count * 200
        };
        let mut results = Vec::with_capacity(capacity);
        self.scan_page_rows(root_page, &mut results)?;
        Ok(results)
    }

    /// Scan rows with an early-exit limit (for LIMIT pushdown)
    pub fn scan_rows_limit(&mut self, root_page: u32, limit: usize) -> Result<Vec<Row>> {
        let mut results = Vec::with_capacity(limit);
        self.scan_page_rows_limit(root_page, &mut results, limit)?;
        Ok(results)
    }

    /// Recursively scan leaf pages with limit; returns Ok(false) when limit reached
    fn scan_page_rows_limit(
        &mut self,
        page_num: u32,
        results: &mut Vec<Row>,
        limit: usize,
    ) -> Result<bool> {
        let page = self.pager.get_page(page_num)?;
        let data = &page.data;
        let hdr_offset = Self::header_offset(page_num);
        let page_type = data[hdr_offset];
        let cell_count =
            u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap()) as usize;

        match page_type {
            LEAF_TABLE => {
                // Collect cell offsets + raw sizes first to release the immutable borrow,
                // then follow overflow chains (which need mutable pager access).
                let cells: Vec<(usize, u32)> = {
                    let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
                    (0..cell_count)
                        .map(|i| {
                            let ptr_offset = ptr_base + i * 2;
                            let cell_offset = u16::from_le_bytes(
                                data[ptr_offset..ptr_offset + 2].try_into().unwrap(),
                            ) as usize;
                            let raw_size = u32::from_le_bytes(
                                data[cell_offset..cell_offset + 4].try_into().unwrap(),
                            );
                            (cell_offset, raw_size)
                        })
                        .collect()
                };
                // Drop `data` borrow (it borrows `self.pager` via `page`)
                drop(page);

                for (cell_offset, raw_size) in cells {
                    if results.len() >= limit {
                        return Ok(false);
                    }
                    let page_data = {
                        let page2 = self.pager.get_page(page_num)?;
                        page2.data.clone()
                    };
                    let payload = self.read_cell_payload(raw_size, cell_offset + 12, &page_data)?;
                    let row = deserialize_row(&payload)?;
                    results.push(row);
                }
                Ok(results.len() < limit)
            }
            INTERIOR_TABLE => {
                let mut children = [0u32; 294];
                let child_count = cell_count + 1;
                for i in 0..cell_count {
                    let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + i * 2;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap())
                            as usize;
                    children[i] =
                        u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
                }
                children[cell_count] =
                    u32::from_le_bytes(data[hdr_offset + 6..hdr_offset + 10].try_into().unwrap());
                let _ = data;

                for i in 0..child_count {
                    if !self.scan_page_rows_limit(children[i], results, limit)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Err(KkdbError::BTreeError(format!(
                "unexpected page type: 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Recursively scan leaf pages, collecting only rows (no rowids)
    fn scan_page_rows(&mut self, page_num: u32, results: &mut Vec<Row>) -> Result<()> {
        let page = self.pager.get_page(page_num)?;
        let data = &page.data;
        let hdr_offset = Self::header_offset(page_num);
        let page_type = data[hdr_offset];
        let cell_count =
            u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap()) as usize;

        match page_type {
            LEAF_TABLE => {
                let cells: Vec<(usize, u32)> = {
                    let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
                    (0..cell_count)
                        .map(|i| {
                            let ptr_offset = ptr_base + i * 2;
                            let co = u16::from_le_bytes(
                                data[ptr_offset..ptr_offset + 2].try_into().unwrap(),
                            ) as usize;
                            let rs = u32::from_le_bytes(data[co..co + 4].try_into().unwrap());
                            (co, rs)
                        })
                        .collect()
                };
                drop(page);

                for (cell_offset, raw_size) in cells {
                    let page2 = self.pager.get_page(page_num)?;
                    let page_data = page2.data.clone();
                    drop(page2);
                    let payload = self.read_cell_payload(raw_size, cell_offset + 12, &page_data)?;
                    let row = deserialize_row(&payload)?;
                    results.push(row);
                }
            }
            INTERIOR_TABLE => {
                let mut children = [0u32; 294];
                let child_count = cell_count + 1;
                for i in 0..cell_count {
                    let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + i * 2;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap())
                            as usize;
                    children[i] =
                        u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
                }
                children[cell_count] =
                    u32::from_le_bytes(data[hdr_offset + 6..hdr_offset + 10].try_into().unwrap());
                let _ = data;

                for i in 0..child_count {
                    self.scan_page_rows(children[i], results)?;
                }
            }
            _ => {
                return Err(KkdbError::BTreeError(format!(
                    "unexpected page type: 0x{:02x}",
                    page_type
                )));
            }
        }
        Ok(())
    }

    /// Recursively scan all leaf pages
    fn scan_page(&mut self, page_num: u32, results: &mut Vec<(i64, Row)>) -> Result<()> {
        // Single page access: read header + data inline
        let page = self.pager.get_page(page_num)?;
        let data = &page.data;
        let hdr_offset = Self::header_offset(page_num);
        let page_type = data[hdr_offset];
        let cell_count =
            u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap()) as usize;

        match page_type {
            LEAF_TABLE => {
                let cells: Vec<(usize, u32, i64)> = {
                    let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
                    (0..cell_count)
                        .map(|i| {
                            let ptr_offset = ptr_base + i * 2;
                            let co = u16::from_le_bytes(
                                data[ptr_offset..ptr_offset + 2].try_into().unwrap(),
                            ) as usize;
                            let rs = u32::from_le_bytes(data[co..co + 4].try_into().unwrap());
                            let rid = i64::from_le_bytes(data[co + 4..co + 12].try_into().unwrap());
                            (co, rs, rid)
                        })
                        .collect()
                };
                drop(page);

                for (cell_offset, raw_size, rowid) in cells {
                    let page2 = self.pager.get_page(page_num)?;
                    let page_data = page2.data.clone();
                    drop(page2);
                    let payload = self.read_cell_payload(raw_size, cell_offset + 12, &page_data)?;
                    let row = deserialize_row(&payload)?;
                    results.push((rowid, row));
                }
            }
            INTERIOR_TABLE => {
                // Extract child page numbers into stack buffer, then drop page borrow before recursing
                let mut children = [0u32; 294]; // max children per 4KB interior page
                let child_count = cell_count + 1;
                for i in 0..cell_count {
                    let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + i * 2;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap())
                            as usize;
                    children[i] =
                        u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
                }
                children[cell_count] =
                    u32::from_le_bytes(data[hdr_offset + 6..hdr_offset + 10].try_into().unwrap());
                let _ = data;

                for i in 0..child_count {
                    self.scan_page(children[i], results)?;
                }
            }
            _ => {
                return Err(KkdbError::BTreeError(format!(
                    "unexpected page type: 0x{:02x}",
                    page_type
                )));
            }
        }

        Ok(())
    }

    /// Find a row by rowid
    pub fn find_by_rowid(
        &mut self,
        root_page: u32,
        target_rowid: i64,
    ) -> Result<Option<(i64, Row)>> {
        // Single page access: read header + binary search + deserialize
        let page = self.pager.get_page(root_page)?;
        let data = &page.data;
        let hdr_offset = Self::header_offset(root_page);
        let page_type = data[hdr_offset];
        let cell_count =
            u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().unwrap()) as usize;

        match page_type {
            LEAF_TABLE => {
                let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
                let mut lo = 0usize;
                let mut hi = cell_count;
                let mut found_cell: Option<(usize, u32)> = None;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let mid_ptr = ptr_base + mid * 2;
                    let mid_cell_off =
                        u16::from_le_bytes(data[mid_ptr..mid_ptr + 2].try_into().unwrap()) as usize;
                    let mid_rowid = i64::from_le_bytes(
                        data[mid_cell_off + 4..mid_cell_off + 12].try_into().unwrap(),
                    );
                    if mid_rowid == target_rowid {
                        let raw_size = u32::from_le_bytes(
                            data[mid_cell_off..mid_cell_off + 4].try_into().unwrap(),
                        );
                        found_cell = Some((mid_cell_off, raw_size));
                        break;
                    } else if mid_rowid < target_rowid {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                drop(page);
                if let Some((cell_off, raw_size)) = found_cell {
                    let page2 = self.pager.get_page(root_page)?;
                    let page_data = page2.data.clone();
                    drop(page2);
                    let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                    let row = deserialize_row(&payload)?;
                    return Ok(Some((target_rowid, row)));
                }
                Ok(None)
            }
            INTERIOR_TABLE => {
                let child = self.find_child_page(root_page, target_rowid)?;
                self.find_by_rowid(child, target_rowid)
            }
            _ => Err(KkdbError::BTreeError(format!(
                "unexpected page type: 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Delete a row by rowid. Returns (deleted, root_page).
    /// root_page may change in future if rebalancing is implemented.
    pub fn delete_by_rowid(&mut self, root_page: u32, target_rowid: i64) -> Result<(bool, u32)> {
        let deleted = self.delete_from_page(root_page, target_rowid)?;
        // Future: if root becomes empty interior, collapse to child
        Ok((deleted, root_page))
    }

    /// Internal delete - returns whether the row was found and removed
    fn delete_from_page(&mut self, page_num: u32, target_rowid: i64) -> Result<bool> {
        // Read page type to decide branch
        let page = self.pager.get_page(page_num)?;
        let hdr_offset = Self::header_offset(page_num);
        let page_type = page.data[hdr_offset];

        match page_type {
            LEAF_TABLE => {
                // Single page access: binary search + remove in place
                let page = self.pager.get_page_mut(page_num)?;
                let off = hdr_offset;
                let cell_count =
                    u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize;
                let ptr_base = hdr_offset + LEAF_HEADER_SIZE;

                let mut lo = 0usize;
                let mut hi = cell_count;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let mid_ptr = ptr_base + mid * 2;
                    let mid_cell_off =
                        u16::from_le_bytes(page.data[mid_ptr..mid_ptr + 2].try_into().unwrap())
                            as usize;
                    let mid_rowid = i64::from_le_bytes(
                        page.data[mid_cell_off + 4..mid_cell_off + 12]
                            .try_into()
                            .unwrap(),
                    );
                    if mid_rowid == target_rowid {
                        // Check if cell has overflow chain — need to free it
                        let raw_size = u32::from_le_bytes(
                            page.data[mid_cell_off..mid_cell_off + 4].try_into().unwrap(),
                        );
                        let overflow_first = if raw_size & OVERFLOW_FLAG != 0 {
                            Some(u32::from_le_bytes(
                                page.data[mid_cell_off + 16..mid_cell_off + 20].try_into().unwrap(),
                            ))
                        } else {
                            None
                        };

                        // Remove cell from pointer array + update header
                        for i in mid..cell_count - 1 {
                            let src = ptr_base + (i + 1) * 2;
                            let dst = ptr_base + i * 2;
                            let val =
                                u16::from_le_bytes(page.data[src..src + 2].try_into().unwrap());
                            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
                        }
                        page.data[off + 1..off + 3]
                            .copy_from_slice(&((cell_count - 1) as u16).to_le_bytes());

                        // Must drop mutable borrow of page before calling free_overflow_chain
                        drop(page);

                        // Free overflow pages if any (TODO S2: use pager.free_page)
                        if let Some(first_overflow) = overflow_first {
                            self.free_overflow_chain(first_overflow)?;
                        }

                        return Ok(true);
                    } else if mid_rowid < target_rowid {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                Ok(false)
            }
            INTERIOR_TABLE => {
                let child = self.find_child_page(page_num, target_rowid)?;
                self.delete_from_page(child, target_rowid)
            }
            _ => Err(KkdbError::BTreeError(format!(
                "unexpected page type: 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Update a row (delete + re-insert), returns the (possibly new) root page
    pub fn update_row(&mut self, root_page: u32, rowid: i64, new_row: &Row) -> Result<u32> {
        let (_, current_root) = self.delete_by_rowid(root_page, rowid)?;
        self.insert(current_root, rowid, new_row)
    }

    /// Update a row using a reusable serialize buffer
    pub fn update_row_with_buf(
        &mut self,
        root_page: u32,
        rowid: i64,
        new_row: &Row,
        buf: &mut Vec<u8>,
    ) -> Result<u32> {
        let (_, current_root) = self.delete_by_rowid(root_page, rowid)?;
        self.insert_with_buf(current_root, rowid, new_row, buf)
    }

    /// Get the maximum rowid in a table (for autoincrement)
    pub fn max_rowid(&mut self, root_page: u32) -> Result<i64> {
        let page = self.pager.get_page(root_page)?;
        let data = &page.data;
        let off = Self::header_offset(root_page);
        let page_type = data[off];
        let cell_count = u16::from_le_bytes(data[off + 1..off + 3].try_into().unwrap()) as usize;

        match page_type {
            LEAF_TABLE => {
                if cell_count == 0 {
                    return Ok(0);
                }
                let ptr_base = off + LEAF_HEADER_SIZE;
                let last_ptr = ptr_base + (cell_count - 1) * 2;
                let cell_off =
                    u16::from_le_bytes(data[last_ptr..last_ptr + 2].try_into().unwrap()) as usize;
                Ok(i64::from_le_bytes(
                    data[cell_off + 4..cell_off + 12].try_into().unwrap(),
                ))
            }
            INTERIOR_TABLE => {
                let right_child = u32::from_le_bytes(data[off + 6..off + 10].try_into().unwrap());
                self.max_rowid(right_child)
            }
            _ => Err(KkdbError::BTreeError(format!(
                "unexpected page type: 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Count total rows
    pub fn count_rows(&mut self, root_page: u32) -> Result<u64> {
        let page = self.pager.get_page(root_page)?;
        let data = &page.data;
        let off = Self::header_offset(root_page);
        let page_type = data[off];
        let cell_count = u16::from_le_bytes(data[off + 1..off + 3].try_into().unwrap()) as usize;

        match page_type {
            LEAF_TABLE => Ok(cell_count as u64),
            INTERIOR_TABLE => {
                let mut children = [0u32; 294];
                let child_count = cell_count + 1;
                for i in 0..cell_count {
                    let ptr_offset = off + INTERIOR_HEADER_SIZE + i * 2;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap())
                            as usize;
                    children[i] =
                        u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
                }
                children[cell_count] =
                    u32::from_le_bytes(data[off + 6..off + 10].try_into().unwrap());

                let mut total = 0u64;
                for i in 0..child_count {
                    total += self.count_rows(children[i])?;
                }
                Ok(total)
            }
            _ => Ok(0),
        }
    }
}

/// Result of an internal insert operation
enum InsertResult {
    /// Insert completed without splitting
    Done,
    /// Page was split: divider_key should be promoted, new_page is the right half
    Split { divider_key: i64, new_page: u32 },
}

#[derive(Debug)]
struct PageHeader {
    page_type: u8,
    cell_count: u16,
    cell_content_offset: u16,
    right_child: Option<u32>,
}

#[cfg(test)]
#[path = "btree_tests.rs"]
mod tests;
