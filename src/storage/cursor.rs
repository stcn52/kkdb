use crate::error::{KkdbError, Result};
use crate::storage::pager::Pager;
use crate::types::{deserialize_row, Row};

const LEAF_TABLE: u8 = 0x0D;
const INTERIOR_TABLE: u8 = 0x05;
const LEAF_HEADER_SIZE: usize = 10; // 1+2+2+1+4 (includes Q1 next_leaf field)
const INTERIOR_HEADER_SIZE: usize = 10;

/// A cursor for iterating over B-tree rows
/// Maintains a stack of (page_num, cell_index) for traversal
pub struct Cursor {
    /// Stack of (page_num, cell_index) from root to current leaf
    stack: Vec<(u32, usize)>,
    /// Whether cursor is past the end
    pub end_of_table: bool,
}

impl Cursor {
    /// Create a cursor pointing to the first row
    pub fn table_start(pager: &mut Pager, root_page: u32) -> Result<Self> {
        let mut cursor = Cursor {
            stack: Vec::new(),
            end_of_table: false,
        };
        cursor.move_to_leftmost(pager, root_page)?;
        // Check if table is empty
        if let Some(&(page_num, _)) = cursor.stack.last() {
            let page = pager.get_page(page_num)?;
            let off = Self::header_offset(page_num);
            let cell_count = u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap());
            if cell_count == 0 {
                cursor.end_of_table = true;
            }
        }
        Ok(cursor)
    }

    fn header_offset(page_num: u32) -> usize {
        let _ = page_num;
        0
    }

    /// Move cursor to the leftmost leaf
    fn move_to_leftmost(&mut self, pager: &mut Pager, page_num: u32) -> Result<()> {
        // Read page info and extract child before recursing (to release borrow)
        let (page_type, child_page) = {
            let page = pager.get_page(page_num)?;
            let data = &page.data;
            let off = Self::header_offset(page_num);
            let pt = data[off];

            let child = if pt == INTERIOR_TABLE {
                let cell_count = u16::from_le_bytes(data[off + 1..off + 3].try_into().unwrap());
                if cell_count == 0 {
                    u32::from_le_bytes(data[off + 6..off + 10].try_into().unwrap())
                } else {
                    let ptr_offset = off + INTERIOR_HEADER_SIZE;
                    let cell_offset =
                        u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap())
                            as usize;
                    u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap())
                }
            } else {
                0
            };
            (pt, child)
        };

        match page_type {
            LEAF_TABLE => {
                self.stack.push((page_num, 0));
                Ok(())
            }
            INTERIOR_TABLE => {
                self.stack.push((page_num, 0));
                self.move_to_leftmost(pager, child_page)
            }
            _ => Err(KkdbError::BTreeError(format!(
                "invalid page type 0x{:02x}",
                page_type
            ))),
        }
    }

    /// Overflow flag: high bit of the on-disk `payload_size` field.
    /// When set, the cell carries an overflow chain instead of an inline payload.
    const OVERFLOW_FLAG: u32 = 0x8000_0000;

    /// Bytes of actual data per overflow page (PAGE_SIZE - 4 for the next-pointer).
    const OVERFLOW_DATA_SIZE: usize = crate::storage::pager::PAGE_SIZE - 4;

    /// Read the full payload for a cell, following the overflow chain when necessary.
    ///
    /// `raw_payload_size` is the raw u32 read from disk (OVERFLOW_FLAG may be set).
    /// `inline_start` is the byte offset inside `page_data` where the inline region begins
    /// (i.e. cell_offset + 12, after the 4-byte size field and 8-byte rowid).
    ///
    /// Returns the complete, reassembled payload bytes.
    ///
    /// # Fix #6
    /// The previous implementation cast the raw u32 directly to `usize` and used it as a
    /// slice length. When OVERFLOW_FLAG (bit 31) is set this produces a length of ~2 GB,
    /// causing an immediate out-of-bounds panic. This function correctly strips the flag
    /// and follows the overflow chain instead.
    fn read_cell_payload(
        pager: &mut Pager,
        raw_payload_size: u32,
        inline_start: usize,
        page_data: &[u8],
    ) -> Result<Vec<u8>> {
        if raw_payload_size & Self::OVERFLOW_FLAG == 0 {
            // Normal inline cell — payload is fully contained in the leaf page.
            let len = raw_payload_size as usize;
            return Ok(page_data[inline_start..inline_start + len].to_vec());
        }

        // Overflow cell layout at `inline_start`:
        //   [total_payload_len : u32]          bytes 0-3
        //   [overflow_first_page: u32]          bytes 4-7
        //   [inline_prefix     : inline_len b]  bytes 8 .. 8+inline_len
        //
        // inline_len = bits 0-30 of raw_payload_size (always 0 in current writer, kept for compat)
        let inline_len = (raw_payload_size & !Self::OVERFLOW_FLAG) as usize;
        let total_len = u32::from_le_bytes(
            page_data[inline_start..inline_start + 4].try_into().unwrap(),
        ) as usize;
        let overflow_page = u32::from_le_bytes(
            page_data[inline_start + 4..inline_start + 8].try_into().unwrap(),
        );

        let mut result = Vec::with_capacity(total_len);
        // Prepend any inline prefix bytes (currently always 0, but handle for correctness)
        result.extend_from_slice(&page_data[inline_start + 8..inline_start + 8 + inline_len]);

        // Walk the overflow page chain until we have all `total_len` bytes.
        let mut cur_page = overflow_page;
        while cur_page != 0 && result.len() < total_len {
            let remaining = total_len - result.len();
            let page = pager.get_page(cur_page)?;
            let next_page = u32::from_le_bytes(page.data[0..4].try_into().unwrap());
            let to_copy = remaining.min(Self::OVERFLOW_DATA_SIZE);
            result.extend_from_slice(&page.data[4..4 + to_copy]);
            cur_page = next_page;
        }
        Ok(result)
    }

    /// Get the current row (rowid, row data).
    ///
    /// Handles both normal inline cells and overflow cells.
    pub fn current(&self, pager: &mut Pager) -> Result<(i64, Row)> {
        if self.end_of_table {
            return Err(KkdbError::BTreeError("cursor past end of table".into()));
        }

        let &(page_num, cell_idx) = self
            .stack
            .last()
            .ok_or_else(|| KkdbError::BTreeError("empty cursor stack".into()))?;

        // Step 1 — read the cell header fields from the page, then release the borrow so
        //           read_cell_payload can call pager.get_page() for overflow pages.
        let (raw_payload_size, rowid, inline_start) = {
            let page = pager.get_page(page_num)?;
            let data = &page.data;
            let off = Self::header_offset(page_num);
            let ptr_offset = off + LEAF_HEADER_SIZE + cell_idx * 2;
            let cell_offset =
                u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap()) as usize;

            let raw = u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap());
            let rid =
                i64::from_le_bytes(data[cell_offset + 4..cell_offset + 12].try_into().unwrap());
            (raw, rid, cell_offset + 12)
        };

        // Step 2 — re-fetch the page data snapshot for the inline region.
        //           Cloning only the 4 KB page array is unavoidable here because
        //           read_cell_payload needs &mut Pager to follow overflow chains.
        let page_data = pager.get_page(page_num)?.data.to_vec();

        // Step 3 — deserialize, handling normal and overflow cells uniformly.
        let payload = Self::read_cell_payload(pager, raw_payload_size, inline_start, &page_data)?;
        let row = deserialize_row(&payload)?;

        Ok((rowid, row))
    }

    /// Advance cursor to next row
    pub fn advance(&mut self, pager: &mut Pager) -> Result<()> {
        if self.end_of_table {
            return Ok(());
        }

        let &(page_num, cell_idx) = self
            .stack
            .last()
            .ok_or_else(|| KkdbError::BTreeError("empty cursor stack".into()))?;

        let cell_count = {
            let page = pager.get_page(page_num)?;
            let off = Self::header_offset(page_num);
            u16::from_le_bytes(page.data[off + 1..off + 3].try_into().unwrap()) as usize
        };

        if cell_idx + 1 < cell_count {
            // Move to next cell in this leaf
            if let Some(last) = self.stack.last_mut() {
                last.1 = cell_idx + 1;
            }
        } else {
            // Need to go up and find the next subtree
            self.stack.pop();
            loop {
                if self.stack.is_empty() {
                    self.end_of_table = true;
                    return Ok(());
                }

                let &(parent_page, parent_idx) = self.stack.last().unwrap();
                // Extract needed info from parent page, then release borrow
                let (_parent_cell_count, child_page_opt) = {
                    let parent = pager.get_page(parent_page)?;
                    let parent_data = &parent.data;
                    let parent_off = Self::header_offset(parent_page);
                    let pcc = u16::from_le_bytes(
                        parent_data[parent_off + 1..parent_off + 3]
                            .try_into()
                            .unwrap(),
                    ) as usize;

                    let child = if parent_idx < pcc {
                        let next_idx = parent_idx + 1;
                        Some(if next_idx < pcc {
                            let ptr_offset = parent_off + INTERIOR_HEADER_SIZE + next_idx * 2;
                            let cell_offset = u16::from_le_bytes(
                                parent_data[ptr_offset..ptr_offset + 2].try_into().unwrap(),
                            ) as usize;
                            u32::from_le_bytes(
                                parent_data[cell_offset..cell_offset + 4]
                                    .try_into()
                                    .unwrap(),
                            )
                        } else {
                            u32::from_le_bytes(
                                parent_data[parent_off + 6..parent_off + 10]
                                    .try_into()
                                    .unwrap(),
                            )
                        })
                    } else {
                        None
                    };
                    (pcc, child)
                };

                if let Some(child_page) = child_page_opt {
                    let next_idx = parent_idx + 1;
                    if let Some(last) = self.stack.last_mut() {
                        last.1 = next_idx;
                    }
                    self.move_to_leftmost(pager, child_page)?;
                    return Ok(());
                } else {
                    self.stack.pop();
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
