use crate::error::{KkdbError, Result};
use crate::storage::pager::{
    page_size_to_u16, page_size_to_u16_val, u16_to_page_size, Pager, PAGE_SIZE,
};
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
/// Leaf page (LEAF_TABLE = 0x0D):
/// [0]      page_type (1 byte)
/// [1..3]   cell_count (2 bytes, u16 LE)
/// [3..5]   cell_content_offset (2 bytes, u16 LE) - start of cell content area
/// [5]      fragmented_free_bytes (1 byte)
/// [6..10]  next_leaf_page (4 bytes, u32 LE) — 0 = no next leaf
/// [10..14] prev_leaf_page (4 bytes, u32 LE) — 0 = no prev leaf (doubly-linked list)
///
/// Interior page (INTERIOR_TABLE = 0x05):
/// [0]      page_type (1 byte)
/// [1..3]   cell_count (2 bytes, u16 LE)
/// [3..5]   cell_content_offset (2 bytes, u16 LE)
/// [5]      fragmented_free_bytes (1 byte)
/// [6..10]  right_child_page (4 bytes, u32 LE)
///
/// After header: cell pointer array (2 bytes per pointer, u16 LE)
const LEAF_HEADER_SIZE: usize = 14; // 1+2+2+1+4+4 (includes next_leaf + prev_leaf fields)
const INTERIOR_HEADER_SIZE: usize = 10;

/// Maximum number of child pointers per interior page (computed from PAGE_SIZE).
/// Each cell needs at least 2 bytes (pointer) + 4 bytes (child_page) + 8 bytes (rowid) = 14.
/// Plus 1 extra for the right_child.
const MAX_INTERIOR_CHILDREN: usize = (PAGE_SIZE - INTERIOR_HEADER_SIZE) / 14 + 2;

/// B-Tree operations on the pager
pub struct BTree<'a> {
    pub pager: &'a mut Pager,
}

/// Validates that `cell_offset + needed` fits within `PAGE_SIZE`.
/// Returns `cell_offset` on success or a `CorruptDatabase` error if out of bounds.
#[inline]
fn validate_cell_offset(cell_offset: usize, needed: usize) -> Result<usize> {
    if cell_offset + needed > PAGE_SIZE {
        return Err(KkdbError::CorruptDatabase(format!(
            "cell offset {} + {} exceeds page size {}",
            cell_offset, needed, PAGE_SIZE
        )));
    }
    Ok(cell_offset)
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
                                // [6..10]: next_leaf (leaf) or right_child (interior) — both encoded as u32
        let ext_value: u32 = header.right_child.unwrap_or(0);
        page.data[off + 6..off + 10].copy_from_slice(&ext_value.to_le_bytes());
        // [10..14]: prev_leaf for leaf pages (zeroed for interior pages)
        if header.page_type == LEAF_TABLE {
            page.data[off + 10..off + 14].copy_from_slice(&0u32.to_le_bytes());
        }

        Ok(())
    }

    // ── leaf linked-list helpers (doubly-linked) ──────────────────────────

    /// Read the `next_leaf` pointer from a leaf page header (bytes [6..10]).
    fn get_next_leaf(&mut self, page_num: u32) -> Result<u32> {
        let page = self.pager.get_page(page_num)?;
        let off = Self::header_offset(page_num);
        Ok(u32::from_le_bytes(
            page.data[off + 6..off + 10]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid next_leaf field".into()))?,
        ))
    }

    /// Write the `next_leaf` pointer into a leaf page header.
    fn set_next_leaf(&mut self, page_num: u32, next: u32) -> Result<()> {
        let off = Self::header_offset(page_num);
        let page = self.pager.get_page_mut(page_num)?;
        page.data[off + 6..off + 10].copy_from_slice(&next.to_le_bytes());
        Ok(())
    }

    /// Read the `prev_leaf` pointer from a leaf page header (bytes [10..14]).
    fn get_prev_leaf(&mut self, page_num: u32) -> Result<u32> {
        let page = self.pager.get_page(page_num)?;
        let off = Self::header_offset(page_num);
        Ok(u32::from_le_bytes(
            page.data[off + 10..off + 14]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid prev_leaf field".into()))?,
        ))
    }

    /// Write the `prev_leaf` pointer into a leaf page header.
    fn set_prev_leaf(&mut self, page_num: u32, prev: u32) -> Result<()> {
        let off = Self::header_offset(page_num);
        let page = self.pager.get_page_mut(page_num)?;
        page.data[off + 10..off + 14].copy_from_slice(&prev.to_le_bytes());
        Ok(())
    }

    /// Initialize a new page as a leaf table page
    pub fn init_leaf_page(&mut self, page_num: u32) -> Result<()> {
        let header = PageHeader {
            page_type: LEAF_TABLE,
            cell_count: 0,
            cell_content_offset: page_size_to_u16(),
            right_child: None,
        };
        self.write_page_header(page_num, &header)
    }

    /// Initialize a new page as an interior table page
    fn init_interior_page(&mut self, page_num: u32, right_child: u32) -> Result<()> {
        let header = PageHeader {
            page_type: INTERIOR_TABLE,
            cell_count: 0,
            cell_content_offset: page_size_to_u16(),
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
                self.pager.allocate_page()?
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
            if inline_start + len > page_data.len() {
                return Err(KkdbError::CorruptDatabase(format!(
                    "inline payload extends beyond page boundary: offset {} + len {} > page_size {}",
                    inline_start, len, page_data.len()
                )));
            }
            return Ok(page_data[inline_start..inline_start + len].to_vec());
        }

        // Overflow cell layout (after rowid, before overflow ptr):
        //   [inline_len: u32-without-flag (0 inline bytes stored here, flag already stripped)]
        //   [total_payload_len: u32]
        //   [overflow_first_page: u32]
        //   [inline_bytes: inline_len bytes]  (always 0 in current impl)
        let inline_len = (raw_payload_size & !OVERFLOW_FLAG) as usize;
        if inline_start + 8 > page_data.len() {
            return Err(KkdbError::CorruptDatabase(
                "overflow header extends beyond page boundary".into(),
            ));
        }
        let total_len = u32::from_le_bytes(
            page_data[inline_start..inline_start + 4]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid overflow total_len".into()))?,
        ) as usize;
        let overflow_page = u32::from_le_bytes(
            page_data[inline_start + 4..inline_start + 8]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid overflow page pointer".into()))?,
        );

        let mut result = Vec::with_capacity(total_len);
        // Copy any inline prefix
        if inline_start + 8 + inline_len > page_data.len() {
            return Err(KkdbError::CorruptDatabase(
                "overflow inline bytes extend beyond page boundary".into(),
            ));
        }
        result.extend_from_slice(&page_data[inline_start + 8..inline_start + 8 + inline_len]);

        // Follow overflow chain
        let mut cur_page = overflow_page;
        while cur_page != 0 && result.len() < total_len {
            let remaining = total_len - result.len();
            let page = self.pager.get_page(cur_page)?;
            let next_page =
                u32::from_le_bytes(page.data[0..4].try_into().map_err(|_| {
                    KkdbError::CorruptDatabase("invalid overflow next pointer".into())
                })?);
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
                u32::from_le_bytes(page.data[0..4].try_into().map_err(|_| {
                    KkdbError::CorruptDatabase("invalid overflow next pointer".into())
                })?)
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
        // Guard against payload sizes that would truncate when cast to u32
        if payload.len() > u32::MAX as usize {
            return Err(KkdbError::BTreeError(format!(
                "payload too large: {} bytes exceeds u32::MAX",
                payload.len()
            )));
        }
        // Single page access: read header inline to determine page type and available space
        let (page_type, cell_count, cell_content_offset, _right_child) = {
            let page = self.pager.get_page(page_num)?;
            let off = Self::header_offset(page_num);
            let pt = page.data[off];
            let cc = u16::from_le_bytes(
                page.data[off + 1..off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            );
            let cco = u16::from_le_bytes(page.data[off + 3..off + 5].try_into().map_err(|_| {
                KkdbError::CorruptDatabase("invalid cell_content_offset field".into())
            })?);
            let rc = if pt == INTERIOR_TABLE {
                Some(u32::from_le_bytes(
                    page.data[off + 6..off + 10].try_into().map_err(|_| {
                        KkdbError::CorruptDatabase("invalid right_child field".into())
                    })?,
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
                let cell_len = if needs_overflow {
                    12 + 8
                } else {
                    12 + payload.len()
                };

                let hdr_offset = Self::header_offset(page_num);
                let ptr_array_end = hdr_offset + LEAF_HEADER_SIZE + (cell_count as usize) * 2;
                let available = u16_to_page_size(cell_content_offset).saturating_sub(ptr_array_end);

                if cell_len + 2 <= available {
                    // Fast path: write directly to page
                    if needs_overflow {
                        // Write overflow chain first, then inline cell stub
                        let overflow_page = self.write_overflow_chain(payload)?;
                        self.insert_overflow_cell_raw(
                            page_num,
                            rowid,
                            payload.len() as u32,
                            overflow_page,
                        )?;
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
                            let cc = u16::from_le_bytes(
                                page.data[off + 1..off + 3].try_into().map_err(|_| {
                                    KkdbError::CorruptDatabase("invalid cell_count field".into())
                                })?,
                            ) as usize;
                            let cco = u16_to_page_size(u16::from_le_bytes(
                                page.data[off + 3..off + 5].try_into().map_err(|_| {
                                    KkdbError::CorruptDatabase(
                                        "invalid cell_content_offset field".into(),
                                    )
                                })?,
                            ));
                            let ptr_end = off + INTERIOR_HEADER_SIZE + cc * 2;
                            cco.saturating_sub(ptr_end)
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
    fn leaf_insert_index(
        page_data: &[u8],
        ptr_base: usize,
        cell_count: usize,
        rowid: i64,
    ) -> Result<usize> {
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_ptr = ptr_base + mid * 2;
            let mid_cell_off = u16::from_le_bytes(
                page_data[mid_ptr..mid_ptr + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            ) as usize;
            let mid_rowid = i64::from_le_bytes(
                page_data[mid_cell_off + 4..mid_cell_off + 12]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid rowid field".into()))?,
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
    ) -> Result<()> {
        let hdr_size = LEAF_HEADER_SIZE;
        let ptr_base = hdr_offset + hdr_size;
        let cell_content_offset = u16_to_page_size(u16::from_le_bytes(
            page.data[hdr_offset + 3..hdr_offset + 5]
                .try_into()
                .map_err(|_| {
                    KkdbError::CorruptDatabase("invalid cell_content_offset field".into())
                })?,
        ));
        if cell_content_offset < cell_data.len() {
            return Err(KkdbError::CorruptDatabase(
                "leaf page overflow: cell_content_offset < cell size".into(),
            ));
        }
        let new_offset = cell_content_offset - cell_data.len();

        // Write cell bytes
        page.data[new_offset..new_offset + cell_data.len()].copy_from_slice(cell_data);

        // Shift pointers right
        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(
                page.data[src..src + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            );
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        page.data[ptr_base + insert_idx * 2..ptr_base + insert_idx * 2 + 2]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());

        // Update header
        page.data[hdr_offset + 1..hdr_offset + 3]
            .copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[hdr_offset + 3..hdr_offset + 5]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());
        Ok(())
    }

    /// Insert a normal (non-overflow) cell into a leaf page directly (no intermediate Vec).
    /// Fix #8: now delegates the pointer-shift + header-update step to write_cell_to_leaf.
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
        let cell_count = u16::from_le_bytes(
            page.data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;
        let ptr_base = hdr_offset + LEAF_HEADER_SIZE;

        let insert_idx = Self::leaf_insert_index(&page.data, ptr_base, cell_count, rowid)?;

        // Build the 12-byte cell header inline and write header + payload into the page,
        // then let write_cell_to_leaf handle the pointer-array shift and header update.
        let cell_content_offset = u16_to_page_size(u16::from_le_bytes(
            page.data[off + 3..off + 5].try_into().map_err(|_| {
                KkdbError::CorruptDatabase("invalid cell_content_offset field".into())
            })?,
        ));
        if cell_content_offset < cell_len {
            return Err(KkdbError::CorruptDatabase(
                "leaf page overflow: cell_content_offset < cell size".into(),
            ));
        }
        let new_offset = cell_content_offset - cell_len;
        page.data[new_offset..new_offset + 4]
            .copy_from_slice(&(payload.len() as u32).to_le_bytes());
        page.data[new_offset + 4..new_offset + 12].copy_from_slice(&rowid.to_le_bytes());
        page.data[new_offset + 12..new_offset + cell_len].copy_from_slice(payload);

        // Shift pointer array and update header counts / cell_content_offset.
        let ptr_base_end = ptr_base + cell_count * 2;
        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = src + 2;
            let val = u16::from_le_bytes(
                page.data[src..src + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            );
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        let _ = ptr_base_end;
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
        let cell_count = u16::from_le_bytes(
            page.data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;
        let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
        let insert_idx = Self::leaf_insert_index(&page.data, ptr_base, cell_count, rowid)?;

        let cell_content_offset = u16_to_page_size(u16::from_le_bytes(
            page.data[off + 3..off + 5].try_into().map_err(|_| {
                KkdbError::CorruptDatabase("invalid cell_content_offset field".into())
            })?,
        ));
        if cell_content_offset < cell_len {
            return Err(KkdbError::CorruptDatabase(
                "leaf page overflow: cell_content_offset < cell size".into(),
            ));
        }
        let new_offset = cell_content_offset - cell_len;

        page.data[new_offset..new_offset + 4].copy_from_slice(&OVERFLOW_FLAG.to_le_bytes());
        page.data[new_offset + 4..new_offset + 12].copy_from_slice(&rowid.to_le_bytes());
        page.data[new_offset + 12..new_offset + 16]
            .copy_from_slice(&total_payload_len.to_le_bytes());
        page.data[new_offset + 16..new_offset + 20]
            .copy_from_slice(&overflow_first_page.to_le_bytes());

        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(
                page.data[src..src + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            );
            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
        }
        page.data[ptr_base + insert_idx * 2..ptr_base + insert_idx * 2 + 2]
            .copy_from_slice(&(new_offset as u16).to_le_bytes());

        page.data[off + 1..off + 3].copy_from_slice(&((cell_count + 1) as u16).to_le_bytes());
        page.data[off + 3..off + 5].copy_from_slice(&(new_offset as u16).to_le_bytes());

        Ok(())
    }

    /// Insert a cell into a leaf page (assumes it fits) — takes a pre-built cell_data slice.
    ///
    /// Fix #8: uses `leaf_insert_index` + `write_cell_to_leaf` instead of inlining a second
    /// copy of the binary-search and pointer-shift logic.
    fn insert_cell_into_leaf(&mut self, page_num: u32, rowid: i64, cell_data: &[u8]) -> Result<()> {
        let hdr_offset = Self::header_offset(page_num);
        let page = self.pager.get_page_mut(page_num)?;
        let off = hdr_offset;
        let cell_count = u16::from_le_bytes(
            page.data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;
        let ptr_base = hdr_offset + LEAF_HEADER_SIZE;

        let insert_idx = Self::leaf_insert_index(&page.data, ptr_base, cell_count, rowid)?;
        Self::write_cell_to_leaf(page, hdr_offset, cell_data, insert_idx, cell_count)?;
        Ok(())
    }

    /// Split a leaf page and insert a cell — returns Split info to propagate upward.
    /// Maintains the doubly-linked leaf list:
    ///   left.next = right, right.next = old_next,
    ///   right.prev = left, old_next.prev = right (if old_next != 0).
    fn split_leaf(&mut self, page_num: u32, rowid: i64, cell_data: &[u8]) -> Result<InsertResult> {
        // Read old next_leaf and prev_leaf before we clear the page
        let old_next = self.get_next_leaf(page_num)?;
        let old_prev = self.get_prev_leaf(page_num)?;

        // Collect all raw cell bytes at once (may include overflow stubs)
        let mut cells: Vec<(i64, Vec<u8>)> =
            {
                let page = self.pager.get_page(page_num)?;
                let data = &page.data;
                let hdr_offset = Self::header_offset(page_num);
                let cell_count =
                    u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid cell_count field".into()),
                    )?) as usize;
                let ptr_base = hdr_offset + LEAF_HEADER_SIZE;
                let mut v = Vec::with_capacity(cell_count + 1);
                for i in 0..cell_count {
                    let ptr_off = ptr_base + i * 2;
                    let cell_off =
                        u16::from_le_bytes(data[ptr_off..ptr_off + 2].try_into().map_err(|_| {
                            KkdbError::CorruptDatabase("invalid cell pointer".into())
                        })?) as usize;
                    validate_cell_offset(cell_off, 12)?;
                    let raw_size =
                        u32::from_le_bytes(data[cell_off..cell_off + 4].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                        )?);
                    let rid =
                        i64::from_le_bytes(data[cell_off + 4..cell_off + 12].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid rowid field".into()),
                        )?);
                    // Cell length on the page (stub only for overflow cells)
                    let cell_len = if raw_size & OVERFLOW_FLAG != 0 {
                        20 // overflow stub: 4+8+4+4
                    } else {
                        12 + (raw_size as usize)
                    };
                    validate_cell_offset(cell_off, cell_len)?;
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

        // Re-init left page and fill
        self.init_leaf_page(page_num)?;
        for (rid, cell) in &cells[..mid] {
            self.insert_cell_into_leaf(page_num, *rid, cell)?;
        }

        // Allocate + fill right page
        let right_page_num = self.pager.allocate_page()?;
        self.init_leaf_page(right_page_num)?;
        for (rid, cell) in &cells[mid..] {
            self.insert_cell_into_leaf(right_page_num, *rid, cell)?;
        }

        // Wire doubly-linked leaf list:
        //   left(page_num) → right_page_num → old_next
        //   left.prev = old_prev (restored after init_leaf_page zeroed it)
        //   right.prev = left
        //   old_next.prev = right (if old_next != 0)
        self.set_next_leaf(page_num, right_page_num)?;
        self.set_prev_leaf(page_num, old_prev)?;
        self.set_next_leaf(right_page_num, old_next)?;
        self.set_prev_leaf(right_page_num, page_num)?;
        if old_next != 0 {
            self.set_prev_leaf(old_next, right_page_num)?;
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
        let (mut cells, old_right_child) =
            {
                let page = self.pager.get_page(page_num)?;
                let data = &page.data;
                let hdr_offset = Self::header_offset(page_num);
                let cell_count =
                    u16::from_le_bytes(data[hdr_offset + 1..hdr_offset + 3].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid cell_count field".into()),
                    )?) as usize;
                let rc =
                    u32::from_le_bytes(data[hdr_offset + 6..hdr_offset + 10].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid right_child field".into()),
                    )?);
                let mut v = Vec::with_capacity(cell_count + 1);
                for i in 0..cell_count {
                    let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + i * 2;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid cell pointer".into()),
                        )?) as usize;
                    validate_cell_offset(cell_offset, 12)?;
                    let child =
                        u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid child page pointer".into()),
                        )?);
                    let key = i64::from_le_bytes(
                        data[cell_offset + 4..cell_offset + 12]
                            .try_into()
                            .map_err(|_| KkdbError::CorruptDatabase("invalid key field".into()))?,
                    );
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
        for cell in cells.iter().take(mid) {
            self.insert_interior_cell(page_num, cell.1, cell.0)?;
        }

        // Right page (new): cells[mid+1..], right_child = final_right_child
        let new_page = self.pager.allocate_page()?;
        self.init_interior_page(new_page, final_right_child)?;
        for cell in cells.iter().skip(mid + 1) {
            self.insert_interior_cell(new_page, cell.1, cell.0)?;
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
        let cell_count = u16::from_le_bytes(
            page.data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;
        let ptr_base = hdr_offset + INTERIOR_HEADER_SIZE;

        // Find the cell with key == divider_key
        let mut divider_idx = None;
        for i in 0..cell_count {
            let p = ptr_base + i * 2;
            let cell_off = u16::from_le_bytes(
                page.data[p..p + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            ) as usize;
            validate_cell_offset(cell_off, 12)?;
            let key = i64::from_le_bytes(
                page.data[cell_off + 4..cell_off + 12]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid key field".into()))?,
            );
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
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
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
        let cell_count = u16::from_le_bytes(
            page.data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;
        let cell_content_offset = u16_to_page_size(u16::from_le_bytes(
            page.data[off + 3..off + 5].try_into().map_err(|_| {
                KkdbError::CorruptDatabase("invalid cell_content_offset field".into())
            })?,
        ));

        // Binary search for insertion position (inline — avoids per-iteration page lookup)
        let ptr_base = hdr_offset + INTERIOR_HEADER_SIZE;
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_ptr = ptr_base + mid * 2;
            let mid_cell_off = u16::from_le_bytes(
                page.data[mid_ptr..mid_ptr + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            ) as usize;
            let mid_key = i64::from_le_bytes(
                page.data[mid_cell_off + 4..mid_cell_off + 12]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid key field".into()))?,
            );
            if key < mid_key {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let insert_idx = lo;

        // Write cell content
        if cell_content_offset < cell_data.len() {
            return Err(KkdbError::CorruptDatabase(
                "interior page overflow: cell_content_offset < cell size".into(),
            ));
        }
        let new_content_offset = cell_content_offset - cell_data.len();
        page.data[new_content_offset..new_content_offset + cell_data.len()]
            .copy_from_slice(&cell_data);

        // Shift cell pointers to make room at insert_idx
        for i in (insert_idx..cell_count).rev() {
            let src = ptr_base + i * 2;
            let dst = ptr_base + (i + 1) * 2;
            let val = u16::from_le_bytes(
                page.data[src..src + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            );
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

        let cell_count = u16::from_le_bytes(
            data[hdr_offset + 1..hdr_offset + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;
        let right_child = u32::from_le_bytes(
            data[hdr_offset + 6..hdr_offset + 10]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid right_child field".into()))?,
        );

        if cell_count == 0 {
            return Ok(right_child);
        }

        // Binary search: find first cell where key > rowid
        let mut lo = 0usize;
        let mut hi = cell_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + mid * 2;
            let cell_offset = u16::from_le_bytes(
                data[ptr_offset..ptr_offset + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            ) as usize;
            validate_cell_offset(cell_offset, 12)?;
            let key = i64::from_le_bytes(
                data[cell_offset + 4..cell_offset + 12]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid key field".into()))?,
            );

            if rowid < key {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }

        // lo = first cell where key > rowid
        if lo < cell_count {
            let ptr_offset = hdr_offset + INTERIOR_HEADER_SIZE + lo * 2;
            let cell_offset = u16::from_le_bytes(
                data[ptr_offset..ptr_offset + 2]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
            ) as usize;
            validate_cell_offset(cell_offset, 4)?;
            let child =
                u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().map_err(
                    |_| KkdbError::CorruptDatabase("invalid child page pointer".into()),
                )?);
            Ok(child)
        } else {
            Ok(right_child)
        }
    }

    // ── Q1 Leaf chain scan ───────────────────────────────────────────────

    /// Walk interior nodes to find the page number of the leftmost leaf.
    fn find_leftmost_leaf(&mut self, root_page: u32) -> Result<u32> {
        let mut cur = root_page;
        loop {
            let page = self.pager.get_page(cur)?;
            let hdr_off = Self::header_offset(cur);
            if page.data[hdr_off] == LEAF_TABLE {
                return Ok(cur);
            }
            // Interior: leftmost child is encoded in the first pointer-array entry
            let cell_count = u16::from_le_bytes(
                page.data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + INTERIOR_HEADER_SIZE;
            cur =
                if cell_count > 0 {
                    let cell_off =
                        u16::from_le_bytes(page.data[ptr_base..ptr_base + 2].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid cell pointer".into()),
                        )?) as usize;
                    validate_cell_offset(cell_off, 4)?;
                    // Interior cell: [left_child:u32][key:i64]
                    u32::from_le_bytes(page.data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid child page pointer".into()),
                    )?)
                } else {
                    // Empty interior: right_child is the only child
                    u32::from_le_bytes(page.data[hdr_off + 6..hdr_off + 10].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid right_child field".into()),
                    )?)
                };
        }
    }

    /// Scan all (rowid, row) by following the leaf next_leaf linked list.
    /// O(n) and avoids interior node traversal after the initial leftmost-leaf descent.
    fn scan_leaf_chain(&mut self, root_page: u32) -> Result<Vec<(i64, Row)>> {
        let first_leaf = self.find_leftmost_leaf(root_page)?;
        let mut results = Vec::new();
        let mut cur = first_leaf;
        while cur != 0 {
            let page_data = self.pager.get_page(cur)?.data.to_vec();
            let hdr_off = Self::header_offset(cur);
            let cell_count = u16::from_le_bytes(
                page_data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + LEAF_HEADER_SIZE;
            for i in 0..cell_count {
                let ptr_off = ptr_base + i * 2;
                let cell_off = u16::from_le_bytes(
                    page_data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                let raw_size =
                    u32::from_le_bytes(page_data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                    )?);
                let rowid = i64::from_le_bytes(
                    page_data[cell_off + 4..cell_off + 12]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid rowid field".into()))?,
                );
                let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                let row = deserialize_row(&payload)?;
                results.push((rowid, row));
            }
            // Advance to next leaf via the linked list
            let next = u32::from_le_bytes(
                page_data[hdr_off + 6..hdr_off + 10]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid next_leaf field".into()))?,
            );
            cur = next;
        }
        Ok(results)
    }

    /// Scan all rows by following the leaf chain (rows only, no rowids).
    fn scan_leaf_chain_rows(&mut self, root_page: u32) -> Result<Vec<Row>> {
        let first_leaf = self.find_leftmost_leaf(root_page)?;
        let mut results = Vec::new();
        let mut cur = first_leaf;
        while cur != 0 {
            let page_data = self.pager.get_page(cur)?.data.to_vec();
            let hdr_off = Self::header_offset(cur);
            let cell_count = u16::from_le_bytes(
                page_data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + LEAF_HEADER_SIZE;
            for i in 0..cell_count {
                let ptr_off = ptr_base + i * 2;
                let cell_off = u16::from_le_bytes(
                    page_data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                validate_cell_offset(cell_off, 4)?;
                let raw_size =
                    u32::from_le_bytes(page_data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                    )?);
                let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                let row = deserialize_row(&payload)?;
                results.push(row);
            }
            let next = u32::from_le_bytes(
                page_data[hdr_off + 6..hdr_off + 10]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid next_leaf field".into()))?,
            );
            cur = next;
        }
        Ok(results)
    }

    /// Scan rows with an early-exit limit using the leaf chain.
    fn scan_leaf_chain_rows_limit(&mut self, root_page: u32, limit: usize) -> Result<Vec<Row>> {
        let first_leaf = self.find_leftmost_leaf(root_page)?;
        let mut results = Vec::with_capacity(limit);
        let mut cur = first_leaf;
        'outer: while cur != 0 {
            let page_data = self.pager.get_page(cur)?.data.to_vec();
            let hdr_off = Self::header_offset(cur);
            let cell_count = u16::from_le_bytes(
                page_data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + LEAF_HEADER_SIZE;
            for i in 0..cell_count {
                if results.len() >= limit {
                    break 'outer;
                }
                let ptr_off = ptr_base + i * 2;
                let cell_off = u16::from_le_bytes(
                    page_data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                validate_cell_offset(cell_off, 4)?;
                let raw_size =
                    u32::from_le_bytes(page_data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                    )?);
                let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                let row = deserialize_row(&payload)?;
                results.push(row);
            }
            let next = u32::from_le_bytes(
                page_data[hdr_off + 6..hdr_off + 10]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid next_leaf field".into()))?,
            );
            cur = next;
        }
        Ok(results)
    }

    // ── Public scan API ────────────────────────────────────────────────────────

    /// Scan all rows in a table — uses leaf chain scan (O(n), no interior traversal)
    pub fn scan_all(&mut self, root_page: u32) -> Result<Vec<(i64, Row)>> {
        self.scan_leaf_chain(root_page)
    }

    /// Scan all rows without rowids — uses leaf chain scan
    pub fn scan_rows(&mut self, root_page: u32) -> Result<Vec<Row>> {
        self.scan_leaf_chain_rows(root_page)
    }

    /// Scan rows with an early-exit limit — uses leaf chain scan
    pub fn scan_rows_limit(&mut self, root_page: u32, limit: usize) -> Result<Vec<Row>> {
        self.scan_leaf_chain_rows_limit(root_page, limit)
    }

    // ── Backward (reverse) scan via prev_leaf doubly-linked list ─────────────

    /// Walk interior nodes to find the page number of the rightmost leaf.
    fn find_rightmost_leaf(&mut self, page_num: u32) -> Result<u32> {
        let page = self.pager.get_page(page_num)?;
        let data = &page.data;
        let off = Self::header_offset(page_num);
        let page_type = data[off];
        match page_type {
            LEAF_TABLE => Ok(page_num),
            INTERIOR_TABLE => {
                let right_child =
                    u32::from_le_bytes(data[off + 6..off + 10].try_into().map_err(|_| {
                        KkdbError::CorruptDatabase("invalid right_child field".into())
                    })?);
                self.find_rightmost_leaf(right_child)
            }
            _ => Err(KkdbError::BTreeError(format!(
                "unexpected page type: 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Scan all rows in reverse order using the prev_leaf doubly-linked list.
    /// Starts from the rightmost leaf and follows prev_leaf pointers backwards.
    /// Within each leaf, cells are iterated in reverse order.
    pub fn scan_all_reverse(&mut self, root_page: u32) -> Result<Vec<(i64, Row)>> {
        let last_leaf = self.find_rightmost_leaf(root_page)?;
        let mut results = Vec::new();
        let mut cur = last_leaf;
        while cur != 0 {
            let page_data = self.pager.get_page(cur)?.data.to_vec();
            let hdr_off = Self::header_offset(cur);
            let cell_count = u16::from_le_bytes(
                page_data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + LEAF_HEADER_SIZE;
            // Iterate cells in reverse within this leaf
            for i in (0..cell_count).rev() {
                let ptr_off = ptr_base + i * 2;
                let cell_off = u16::from_le_bytes(
                    page_data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                validate_cell_offset(cell_off, 12)?;
                let raw_size =
                    u32::from_le_bytes(page_data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                    )?);
                let rowid = i64::from_le_bytes(
                    page_data[cell_off + 4..cell_off + 12]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid rowid field".into()))?,
                );
                let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                let row = deserialize_row(&payload)?;
                results.push((rowid, row));
            }
            // Follow prev_leaf pointer backwards
            let prev = u32::from_le_bytes(
                page_data[hdr_off + 10..hdr_off + 14]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid prev_leaf field".into()))?,
            );
            cur = prev;
        }
        Ok(results)
    }

    /// Scan rows in reverse with an early-exit limit.
    pub fn scan_rows_reverse_limit(&mut self, root_page: u32, limit: usize) -> Result<Vec<Row>> {
        let last_leaf = self.find_rightmost_leaf(root_page)?;
        let mut results = Vec::with_capacity(limit);
        let mut cur = last_leaf;
        'outer: while cur != 0 {
            let page_data = self.pager.get_page(cur)?.data.to_vec();
            let hdr_off = Self::header_offset(cur);
            let cell_count = u16::from_le_bytes(
                page_data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + LEAF_HEADER_SIZE;
            for i in (0..cell_count).rev() {
                if results.len() >= limit {
                    break 'outer;
                }
                let ptr_off = ptr_base + i * 2;
                let cell_off = u16::from_le_bytes(
                    page_data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                validate_cell_offset(cell_off, 4)?;
                let raw_size =
                    u32::from_le_bytes(page_data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                    )?);
                let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                let row = deserialize_row(&payload)?;
                results.push(row);
            }
            let prev = u32::from_le_bytes(
                page_data[hdr_off + 10..hdr_off + 14]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid prev_leaf field".into()))?,
            );
            cur = prev;
        }
        Ok(results)
    }

    // ── F1: Prefix-compressed index scan ──────────────────────────────────────

    /// Scan all index entries from a prefix-compressed index B-Tree.
    ///
    /// Each leaf page is decoded with a fresh `PrefixPageDecoder` (reset at page boundary).
    /// Returns `(btree_rowid, index_row)` pairs.
    pub fn scan_all_compressed(&mut self, root_page: u32) -> Result<Vec<(i64, Row)>> {
        use crate::types::deserialize_index_row_with_prefix;
        let first_leaf = self.find_leftmost_leaf(root_page)?;
        let mut results = Vec::new();
        let mut cur = first_leaf;
        while cur != 0 {
            let page_data = self.pager.get_page(cur)?.data.to_vec();
            let hdr_off = Self::header_offset(cur);
            let cell_count = u16::from_le_bytes(
                page_data[hdr_off + 1..hdr_off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
            ) as usize;
            let ptr_base = hdr_off + LEAF_HEADER_SIZE;
            // Per-page decoder — reset prefix at each new page
            let mut prev_key: Vec<u8> = Vec::new();
            for i in 0..cell_count {
                let ptr_off = ptr_base + i * 2;
                let cell_off = u16::from_le_bytes(
                    page_data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                validate_cell_offset(cell_off, 12)?;
                let raw_size =
                    u32::from_le_bytes(page_data[cell_off..cell_off + 4].try_into().map_err(
                        |_| KkdbError::CorruptDatabase("invalid payload size field".into()),
                    )?);
                let rowid = i64::from_le_bytes(
                    page_data[cell_off + 4..cell_off + 12]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid rowid field".into()))?,
                );
                let payload = self.read_cell_payload(raw_size, cell_off + 12, &page_data)?;
                let (row, new_prev) = deserialize_index_row_with_prefix(&payload, &prev_key)?;
                prev_key = new_prev;
                results.push((rowid, row));
            }
            let next = u32::from_le_bytes(
                page_data[hdr_off + 6..hdr_off + 10]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("invalid next_leaf field".into()))?,
            );
            cur = next;
        }
        Ok(results)
    }

    /// Insert a prefix-compressed index row into a B-Tree.
    ///
    /// # ⚠️ IMPORTANT: Monotonic / Bulk-Load Only
    ///
    /// Prefix compression is delta-encoded relative to `prev_key` per **page**.
    /// The B-Tree leaf scanner (`scan_all_compressed`) resets the prev_key to `[]`
    /// at every new page. This means:
    ///
    /// - Rows **within the same leaf page** must be inserted in strictly increasing
    ///   key order so that the delta chain is contiguous and decodable.
    /// - After a **page split**, the new right-leaf starts a fresh delta chain from
    ///   `prev=[]`. Since `split_leaf` copies already-encoded payloads verbatim,
    ///   the right-leaf's first cell was encoded relative to some *mid-page* prev_key,
    ///   **not** relative to `[]` — this would cause incorrect decode.
    ///
    /// Therefore `insert_compressed` is **only safe for append-only / bulk-load index
    /// builds** where all keys arrive in sorted order AND no page split occurs mid-stream
    /// for already-compressed pages. For general-purpose index inserts use `btree.insert`
    /// with the standard (uncompressed) serialization instead.
    ///
    /// `prev_key`: the raw text bytes of the previous row's first key column.
    /// Returns `(new_root, new_prev_key)`.
    pub fn insert_compressed(
        &mut self,
        root_page: u32,
        rowid: i64,
        row: &Row,
        prev_key: &[u8],
    ) -> Result<(u32, Vec<u8>)> {
        use crate::types::serialize_index_row_compressed;
        let (payload, new_prev) = serialize_index_row_compressed(row, prev_key);
        let new_root = match self.insert_into_page(root_page, rowid, &payload)? {
            InsertResult::Done => root_page,
            InsertResult::Split {
                divider_key,
                new_page,
            } => {
                // A page split copies already-encoded delta cells verbatim into the
                // right leaf.  The first cell on that new page was encoded relative to
                // a mid-page predecessor, but `scan_all_compressed` resets prev_key to
                // `[]` at every leaf boundary.  Accepting the split would silently
                // corrupt the prefix-compressed index.  Return an explicit error
                // instead.
                //
                // For resilience we still materialise the split so the pager state
                // stays consistent (the right-leaf was already written), but we surface
                // the problem to the caller.
                let new_root = self.pager.allocate_page()?;
                self.init_interior_page(new_root, new_page)?;
                self.insert_interior_cell(new_root, divider_key, root_page)?;
                return Err(KkdbError::BTreeError(
                    "page split occurred during prefix-compressed insert; \
                     compressed index may be corrupt — rebuild the index"
                        .into(),
                ));
            }
        };
        Ok((new_root, new_prev))
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
        let cell_count = u16::from_le_bytes(
            data[hdr_offset + 1..hdr_offset + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;

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
                        u16::from_le_bytes(data[mid_ptr..mid_ptr + 2].try_into().map_err(|_| {
                            KkdbError::CorruptDatabase("invalid cell pointer".into())
                        })?) as usize;
                    validate_cell_offset(mid_cell_off, 12)?;
                    let mid_rowid = i64::from_le_bytes(
                        data[mid_cell_off + 4..mid_cell_off + 12]
                            .try_into()
                            .map_err(|_| {
                                KkdbError::CorruptDatabase("invalid rowid field".into())
                            })?,
                    );
                    if mid_rowid == target_rowid {
                        let raw_size = u32::from_le_bytes(
                            data[mid_cell_off..mid_cell_off + 4]
                                .try_into()
                                .map_err(|_| {
                                    KkdbError::CorruptDatabase("invalid payload size field".into())
                                })?,
                        );
                        found_cell = Some((mid_cell_off, raw_size));
                        break;
                    } else if mid_rowid < target_rowid {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                let _ = page;
                if let Some((cell_off, raw_size)) = found_cell {
                    let page2 = self.pager.get_page(root_page)?;
                    let page_data = page2.data;
                    let _ = page2;
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
                    u16::from_le_bytes(page.data[off + 1..off + 3].try_into().map_err(|_| {
                        KkdbError::CorruptDatabase("invalid cell_count field".into())
                    })?) as usize;
                let ptr_base = hdr_offset + LEAF_HEADER_SIZE;

                let mut lo = 0usize;
                let mut hi = cell_count;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let mid_ptr = ptr_base + mid * 2;
                    let mid_cell_off =
                        u16::from_le_bytes(page.data[mid_ptr..mid_ptr + 2].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid cell pointer".into()),
                        )?) as usize;
                    validate_cell_offset(mid_cell_off, 12)?;
                    let mid_rowid = i64::from_le_bytes(
                        page.data[mid_cell_off + 4..mid_cell_off + 12]
                            .try_into()
                            .map_err(|_| {
                                KkdbError::CorruptDatabase("invalid rowid field".into())
                            })?,
                    );
                    if mid_rowid == target_rowid {
                        // Check if cell has overflow chain — need to free it
                        let raw_size = u32::from_le_bytes(
                            page.data[mid_cell_off..mid_cell_off + 4]
                                .try_into()
                                .map_err(|_| {
                                    KkdbError::CorruptDatabase("invalid payload size field".into())
                                })?,
                        );
                        let overflow_first = if raw_size & OVERFLOW_FLAG != 0 {
                            Some(u32::from_le_bytes(
                                page.data[mid_cell_off + 16..mid_cell_off + 20]
                                    .try_into()
                                    .map_err(|_| {
                                        KkdbError::CorruptDatabase(
                                            "invalid overflow page pointer".into(),
                                        )
                                    })?,
                            ))
                        } else {
                            None
                        };

                        // Remove cell from pointer array + update header
                        for i in mid..cell_count - 1 {
                            let src = ptr_base + (i + 1) * 2;
                            let dst = ptr_base + i * 2;
                            let val =
                                u16::from_le_bytes(page.data[src..src + 2].try_into().map_err(
                                    |_| KkdbError::CorruptDatabase("invalid cell pointer".into()),
                                )?);
                            page.data[dst..dst + 2].copy_from_slice(&val.to_le_bytes());
                        }
                        page.data[off + 1..off + 3]
                            .copy_from_slice(&((cell_count - 1) as u16).to_le_bytes());

                        // Must drop mutable borrow of page before calling free_overflow_chain
                        let _ = page;

                        // Free overflow pages (S2: implemented via pager.free_page)
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
        let cell_count = u16::from_le_bytes(
            data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;

        match page_type {
            LEAF_TABLE => {
                if cell_count == 0 {
                    return Ok(0);
                }
                let ptr_base = off + LEAF_HEADER_SIZE;
                let last_ptr = ptr_base + (cell_count - 1) * 2;
                let cell_off = u16::from_le_bytes(
                    data[last_ptr..last_ptr + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid cell pointer".into()))?,
                ) as usize;
                validate_cell_offset(cell_off, 12)?;
                Ok(i64::from_le_bytes(
                    data[cell_off + 4..cell_off + 12]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("invalid rowid field".into()))?,
                ))
            }
            INTERIOR_TABLE => {
                let right_child =
                    u32::from_le_bytes(data[off + 6..off + 10].try_into().map_err(|_| {
                        KkdbError::CorruptDatabase("invalid right_child field".into())
                    })?);
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
        let cell_count = u16::from_le_bytes(
            data[off + 1..off + 3]
                .try_into()
                .map_err(|_| KkdbError::CorruptDatabase("invalid cell_count field".into()))?,
        ) as usize;

        match page_type {
            LEAF_TABLE => Ok(cell_count as u64),
            INTERIOR_TABLE => {
                let mut children = vec![0u32; MAX_INTERIOR_CHILDREN];
                let child_count = cell_count + 1;
                for (i, child) in children.iter_mut().enumerate().take(cell_count) {
                    let ptr_offset = off + INTERIOR_HEADER_SIZE + i * 2;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid cell pointer".into()),
                        )?) as usize;
                    validate_cell_offset(cell_offset, 4)?;
                    *child =
                        u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().map_err(
                            |_| KkdbError::CorruptDatabase("invalid child page pointer".into()),
                        )?);
                }
                children[cell_count] =
                    u32::from_le_bytes(data[off + 6..off + 10].try_into().map_err(|_| {
                        KkdbError::CorruptDatabase("invalid right_child field".into())
                    })?);

                let mut total = 0u64;
                for child in children.iter().take(child_count) {
                    total += self.count_rows(*child)?;
                }
                Ok(total)
            }
            _ => Ok(0),
        }
    }

    // ── VACUUM / Defragmentation ─────────────────────────────────────────

    /// Collect fragmentation statistics for a B-Tree rooted at `root_page`.
    ///
    /// Returns `(total_leaf_pages, total_fragmented_bytes, total_overflow_pages,
    ///           free_space_bytes)`.
    pub fn fragmentation_stats(&mut self, root_page: u32) -> Result<(u64, u64, u64, u64)> {
        let mut total_leaves = 0u64;
        let mut total_frag = 0u64;
        let mut total_overflow = 0u64;
        let mut total_free = 0u64;
        self.collect_frag_stats(
            root_page,
            &mut total_leaves,
            &mut total_frag,
            &mut total_overflow,
            &mut total_free,
        )?;
        Ok((total_leaves, total_frag, total_overflow, total_free))
    }

    fn collect_frag_stats(
        &mut self,
        page_num: u32,
        leaves: &mut u64,
        frag: &mut u64,
        overflow: &mut u64,
        free: &mut u64,
    ) -> Result<()> {
        let off = Self::header_offset(page_num);
        let (page_type, cell_count, cco, frag_bytes) = {
            let page = self.pager.get_page(page_num)?;
            let pt = page.data[off];
            let cc = u16::from_le_bytes(
                page.data[off + 1..off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad cc".into()))?,
            );
            let co = u16_to_page_size(u16::from_le_bytes(
                page.data[off + 3..off + 5]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad cco".into()))?,
            ));
            let fb = page.data[off + 5];
            (pt, cc as usize, co, fb as u64)
        };

        if page_type == LEAF_TABLE {
            *leaves += 1;
            *frag += frag_bytes;
            let hdr_size = LEAF_HEADER_SIZE;
            let ptr_end = off + hdr_size + cell_count * 2;
            let gap = cco.saturating_sub(ptr_end);
            *free += gap as u64;

            // Count overflow pages per cell
            let mut overflow_starts: Vec<u32> = Vec::new();
            {
                let page = self.pager.get_page(page_num)?;
                for i in 0..cell_count {
                    let ptr_offset = off + hdr_size + i * 2;
                    if ptr_offset + 2 > PAGE_SIZE {
                        break;
                    }
                    let cell_off = u16_to_page_size(u16::from_le_bytes(
                        page.data[ptr_offset..ptr_offset + 2]
                            .try_into()
                            .map_err(|_| KkdbError::CorruptDatabase("bad ptr".into()))?,
                    ));
                    if cell_off + 12 > PAGE_SIZE {
                        continue;
                    }
                    let payload_size =
                        u32::from_le_bytes(page.data[cell_off..cell_off + 4].try_into().unwrap())
                            as usize;
                    if payload_size > PAGE_SIZE - 12 {
                        let first_overflow = u32::from_le_bytes(
                            page.data[cell_off + 8..cell_off + 12].try_into().unwrap(),
                        );
                        overflow_starts.push(first_overflow);
                    }
                }
            }
            for first in overflow_starts {
                *overflow += self.count_overflow_pages(first)?;
            }
        } else if page_type == INTERIOR_TABLE {
            // Recurse into children
            let page = self.pager.get_page(page_num)?;
            let mut children = Vec::with_capacity(cell_count + 1);
            for i in 0..cell_count {
                let ptr_offset = off + INTERIOR_HEADER_SIZE + i * 2;
                if ptr_offset + 2 > PAGE_SIZE {
                    break;
                }
                let cell_off = u16_to_page_size(u16::from_le_bytes(
                    page.data[ptr_offset..ptr_offset + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("bad ptr".into()))?,
                ));
                if cell_off + 4 > PAGE_SIZE {
                    continue;
                }
                let child =
                    u32::from_le_bytes(page.data[cell_off..cell_off + 4].try_into().unwrap());
                children.push(child);
            }
            // Right child
            let rc = u32::from_le_bytes(
                page.data[off + 6..off + 10]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad rc".into()))?,
            );
            children.push(rc);

            for child in children {
                self.collect_frag_stats(child, leaves, frag, overflow, free)?;
            }
        }
        Ok(())
    }

    fn count_overflow_pages(&mut self, first_page: u32) -> Result<u64> {
        let mut count = 0;
        let mut cur = first_page;
        while cur != 0 {
            count += 1;
            let page = self.pager.get_page(cur)?;
            cur = u32::from_le_bytes(
                page.data[0..4]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad overflow ptr".into()))?,
            );
        }
        Ok(count)
    }

    /// Defragment a single leaf page by compacting its cell content area.
    ///
    /// Returns `true` if the page was actually defragmented (had fragments),
    /// `false` if it was already compact.
    pub fn defragment_leaf(&mut self, page_num: u32) -> Result<bool> {
        let off = Self::header_offset(page_num);

        // Read header
        let (page_type, cell_count, frag_bytes) = {
            let page = self.pager.get_page(page_num)?;
            let pt = page.data[off];
            let cc = u16::from_le_bytes(
                page.data[off + 1..off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad cc".into()))?,
            ) as usize;
            let fb = page.data[off + 5];
            (pt, cc, fb)
        };

        if page_type != LEAF_TABLE || frag_bytes == 0 {
            return Ok(false);
        }

        // Collect all cells: (ptr_index, cell_offset, cell_data)
        let mut cells: Vec<(usize, Vec<u8>)> = Vec::with_capacity(cell_count);
        {
            let page = self.pager.get_page(page_num)?;
            for i in 0..cell_count {
                let ptr_off = off + LEAF_HEADER_SIZE + i * 2;
                if ptr_off + 2 > PAGE_SIZE {
                    break;
                }
                let cell_off = u16_to_page_size(u16::from_le_bytes(
                    page.data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("bad ptr".into()))?,
                ));
                // Read cell size: payload_size (4 bytes) tells us the cell length
                if cell_off + 4 > PAGE_SIZE {
                    continue;
                }
                let payload_size =
                    u32::from_le_bytes(page.data[cell_off..cell_off + 4].try_into().unwrap())
                        as usize;
                // Cell format: [payload_size:4][rowid:8][payload or overflow stub]
                let inline_payload = if payload_size > PAGE_SIZE - 12 {
                    4 // overflow stub: just the 4-byte overflow page pointer is inline
                } else {
                    payload_size
                };
                let cell_len = 4 + 8 + inline_payload; // payload_size + rowid + inline data
                let end = (cell_off + cell_len).min(PAGE_SIZE);
                cells.push((i, page.data[cell_off..end].to_vec()));
            }
        }

        // Rewrite the page with compacted cells
        let page = self.pager.get_page_mut(page_num)?;
        // Clear the cell content area (from after ptr array to end of page)
        let ptr_end = off + LEAF_HEADER_SIZE + cell_count * 2;
        page.data[ptr_end..PAGE_SIZE].fill(0);

        // Write cells from the end of the page, compactly
        let mut write_offset = PAGE_SIZE;
        for (i, cell_data) in &cells {
            write_offset -= cell_data.len();
            page.data[write_offset..write_offset + cell_data.len()].copy_from_slice(cell_data);
            // Update pointer
            let ptr_off = off + LEAF_HEADER_SIZE + i * 2;
            page.data[ptr_off..ptr_off + 2]
                .copy_from_slice(&page_size_to_u16_val(write_offset).to_le_bytes());
        }

        // Update header: new cell_content_offset, clear fragmented_free_bytes
        page.data[off + 3..off + 5]
            .copy_from_slice(&page_size_to_u16_val(write_offset).to_le_bytes());
        page.data[off + 5] = 0; // fragmented bytes = 0

        Ok(true)
    }

    /// Defragment all leaf pages in the B-Tree rooted at `root_page`.
    /// Returns the number of pages actually defragmented.
    pub fn defragment_all(&mut self, root_page: u32) -> Result<u64> {
        let off = Self::header_offset(root_page);
        let page_type = {
            let page = self.pager.get_page(root_page)?;
            page.data[off]
        };

        if page_type == LEAF_TABLE {
            Ok(if self.defragment_leaf(root_page)? {
                1
            } else {
                0
            })
        } else if page_type == INTERIOR_TABLE {
            let page = self.pager.get_page(root_page)?;
            let cell_count = u16::from_le_bytes(
                page.data[off + 1..off + 3]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad cc".into()))?,
            ) as usize;
            let mut children = Vec::with_capacity(cell_count + 1);
            for i in 0..cell_count {
                let ptr_off = off + INTERIOR_HEADER_SIZE + i * 2;
                if ptr_off + 2 > PAGE_SIZE {
                    break;
                }
                let cell_off = u16_to_page_size(u16::from_le_bytes(
                    page.data[ptr_off..ptr_off + 2]
                        .try_into()
                        .map_err(|_| KkdbError::CorruptDatabase("bad ptr".into()))?,
                ));
                if cell_off + 4 > PAGE_SIZE {
                    continue;
                }
                let child =
                    u32::from_le_bytes(page.data[cell_off..cell_off + 4].try_into().unwrap());
                children.push(child);
            }
            let rc = u32::from_le_bytes(
                page.data[off + 6..off + 10]
                    .try_into()
                    .map_err(|_| KkdbError::CorruptDatabase("bad rc".into()))?,
            );
            children.push(rc);

            let mut total = 0;
            for child in children {
                total += self.defragment_all(child)?;
            }
            Ok(total)
        } else {
            Ok(0)
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
