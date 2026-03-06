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

    /// Get the current row (rowid, row data)
    pub fn current(&self, pager: &mut Pager) -> Result<(i64, Row)> {
        if self.end_of_table {
            return Err(KkdbError::BTreeError("cursor past end of table".into()));
        }

        let &(page_num, cell_idx) = self
            .stack
            .last()
            .ok_or_else(|| KkdbError::BTreeError("empty cursor stack".into()))?;

        let page = pager.get_page(page_num)?;
        let data = &page.data;
        let off = Self::header_offset(page_num);
        let ptr_offset = off + LEAF_HEADER_SIZE + cell_idx * 2;
        let cell_offset =
            u16::from_le_bytes(data[ptr_offset..ptr_offset + 2].try_into().unwrap()) as usize;

        let payload_size =
            u32::from_le_bytes(data[cell_offset..cell_offset + 4].try_into().unwrap()) as usize;
        let rowid = i64::from_le_bytes(data[cell_offset + 4..cell_offset + 12].try_into().unwrap());
        let payload = &data[cell_offset + 12..cell_offset + 12 + payload_size];
        let row = deserialize_row(payload)?;

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
