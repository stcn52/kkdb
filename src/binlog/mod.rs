use crate::error::{KkdbError, Result};
use crate::types::Row;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Logical operations logged by the binlog
#[derive(Debug, Clone, PartialEq)]
pub enum LogRecord {
    /// Start of a transaction
    Begin(u64),
    /// Insert a row into a table
    Insert {
        txid: u64,
        table_name: String,
        rowid: i64,
        row: Row,
    },
    /// Update a row in a table
    Update {
        txid: u64,
        table_name: String,
        rowid: i64,
        old_row: Row,
        new_row: Row,
    },
    /// Delete a row from a table
    Delete {
        txid: u64,
        table_name: String,
        rowid: i64,
        row: Option<Row>, // The deleted row contents, if available
    },
    /// Prepare to commit the transaction (1st phase of 2PC)
    Prepare(u64),
    /// Commit the transaction (2nd phase of 2PC)
    Commit(u64),
    /// Abort/Rollback the transaction
    Rollback(u64),
}

/// Serializes and deserializes LogRecords.
/// For this implementation, we use a simple binary format or length-prefixed JSON/Bincode.
/// To minimize dependencies, we'll implement a custom binary serialization using the `types::serialize_row` mechanism where applicable.
impl LogRecord {
    // Basic binary serialization/deserialization stubs.
    // Real implementation would safely encode strings and Row values.
    
    pub fn serialize(&self, buf: &mut Vec<u8>) -> Result<()> {
        match self {
            LogRecord::Begin(txid) => {
                buf.push(1);
                crate::varint::write_varint_u64(*txid, buf);
            }
            LogRecord::Insert { txid, table_name, rowid, row } => {
                buf.push(2);
                crate::varint::write_varint_u64(*txid, buf);
                let name_bytes = table_name.as_bytes();
                crate::varint::write_varint_u64(name_bytes.len() as u64, buf);
                buf.write_all(name_bytes).map_err(KkdbError::Io)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                let mut row_buf = Vec::new();
                crate::types::serialize_row_into(row, &mut row_buf);
                crate::varint::write_varint_u64(row_buf.len() as u64, buf);
                buf.write_all(&row_buf).map_err(KkdbError::Io)?;
            }
            LogRecord::Update { txid, table_name, rowid, old_row, new_row } => {
                buf.push(3);
                crate::varint::write_varint_u64(*txid, buf);
                let name_bytes = table_name.as_bytes();
                crate::varint::write_varint_u64(name_bytes.len() as u64, buf);
                buf.write_all(name_bytes).map_err(KkdbError::Io)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                
                let mut old_row_buf = Vec::new();
                crate::types::serialize_row_into(old_row, &mut old_row_buf);
                crate::varint::write_varint_u64(old_row_buf.len() as u64, buf);
                buf.write_all(&old_row_buf).map_err(KkdbError::Io)?;
                
                let mut new_row_buf = Vec::new();
                crate::types::serialize_row_into(new_row, &mut new_row_buf);
                crate::varint::write_varint_u64(new_row_buf.len() as u64, buf);
                buf.write_all(&new_row_buf).map_err(KkdbError::Io)?;
            }
            LogRecord::Delete { txid, table_name, rowid, row } => {
                buf.push(4);
                crate::varint::write_varint_u64(*txid, buf);
                let name_bytes = table_name.as_bytes();
                crate::varint::write_varint_u64(name_bytes.len() as u64, buf);
                buf.write_all(name_bytes).map_err(KkdbError::Io)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                if let Some(r) = row {
                    buf.push(1); // Has row
                    let mut row_buf = Vec::new();
                    crate::types::serialize_row_into(r, &mut row_buf);
                    crate::varint::write_varint_u64(row_buf.len() as u64, buf);
                    buf.write_all(&row_buf).map_err(KkdbError::Io)?;
                } else {
                    buf.push(0); // No row
                }
            }
            LogRecord::Prepare(txid) => {
                buf.push(5);
                crate::varint::write_varint_u64(*txid, buf);
            }
            LogRecord::Commit(txid) => {
                buf.push(6);
                crate::varint::write_varint_u64(*txid, buf);
            }
            LogRecord::Rollback(txid) => {
                buf.push(7);
                crate::varint::write_varint_u64(*txid, buf);
            }
        }
        Ok(())
    }
}

use std::io::BufWriter;

/// Manages the append-only binlog file
pub struct BinlogManager {
    file: Option<BufWriter<File>>, // None if in-memory
}

impl BinlogManager {
    /// Open or create a binlog file for a given database path
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref();
        let binlog_path = db_path.with_extension("binlog");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&binlog_path)
            .map_err(|e| KkdbError::Io(e))?;
        Ok(Self { file: Some(BufWriter::with_capacity(64 * 1024, file)) })
    }
    
    /// Create a dummy binlog manager for in-memory databases
    pub fn open_memory() -> Self {
        Self { file: None }
    }

    /// Append a record to the binlog buffer/file.
    pub fn append(&mut self, record: &LogRecord) -> Result<()> {
        if let Some(file) = &mut self.file {
            let mut record_buf = Vec::new();
            record.serialize(&mut record_buf)?;
            
            // Format: [Total\_Length: u32][Checksum: u32][Record\_Data]
            let total_len = record_buf.len() as u32;
            let checksum = crc32fast::hash(&record_buf);
            
            let mut header = [0u8; 8];
            header[0..4].copy_from_slice(&total_len.to_le_bytes());
            header[4..8].copy_from_slice(&checksum.to_le_bytes());
            
            file.write_all(&header).map_err(KkdbError::Io)?;
            file.write_all(&record_buf).map_err(KkdbError::Io)?;
        }
        Ok(())
    }

    pub fn fsync(&mut self) -> Result<()> {
        if let Some(file) = &mut self.file {
            file.flush().map_err(KkdbError::Io)?;
            file.get_mut().sync_all().map_err(KkdbError::Io)?;
        }
        Ok(())
    }
    
    /// Basic recovery routine placeholder.
    /// In a full implementation, `recover` reads the binlog sequentially,
    /// verifies checksums, extracts the last valid PREPARE / COMMIT records,
    /// and ensures the storage layer generation matches the WAL state.
    pub fn recover(&mut self) -> Result<()> {
        // Currently a no-op placeholder. 
        // Foundation is laid here for full undo/redo automated recovery.
        Ok(())
    }
}
