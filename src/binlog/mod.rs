use crate::error::{KkdbError, Result};
use crate::types::Row;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
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

impl LogRecord {
    // ── Serialization ──────────────────────────────────────────────────────

    pub fn serialize(&self, buf: &mut Vec<u8>) -> Result<()> {
        match self {
            LogRecord::Begin(txid) => {
                buf.push(1);
                crate::varint::write_varint_u64(*txid, buf);
            }
            LogRecord::Insert { txid, table_name, rowid, row } => {
                buf.push(2);
                crate::varint::write_varint_u64(*txid, buf);
                write_string(table_name, buf)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                write_row(row, buf)?;
            }
            LogRecord::Update { txid, table_name, rowid, old_row, new_row } => {
                buf.push(3);
                crate::varint::write_varint_u64(*txid, buf);
                write_string(table_name, buf)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                write_row(old_row, buf)?;
                write_row(new_row, buf)?;
            }
            LogRecord::Delete { txid, table_name, rowid, row } => {
                buf.push(4);
                crate::varint::write_varint_u64(*txid, buf);
                write_string(table_name, buf)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                if let Some(r) = row {
                    buf.push(1);
                    write_row(r, buf)?;
                } else {
                    buf.push(0);
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

    // ── Deserialization ────────────────────────────────────────────────────

    /// Deserialize from a raw byte slice. Returns (record, bytes_consumed) or None if truncated/corrupt.
    pub fn deserialize(data: &[u8], pos: usize) -> Option<(LogRecord, usize)> {
        if pos >= data.len() {
            return None;
        }
        let tag = data[pos];
        let mut off = pos + 1;

        match tag {
            1 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                Some((LogRecord::Begin(txid), off))
            }
            2 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let (table_name, n) = read_string(&data[off..])?;
                off += n;
                let (rowid_enc, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let rowid = crate::varint::zigzag_decode(rowid_enc);
                let (row, n) = read_row(&data[off..])?;
                off += n;
                Some((LogRecord::Insert { txid, table_name, rowid, row }, off))
            }
            3 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let (table_name, n) = read_string(&data[off..])?;
                off += n;
                let (rowid_enc, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let rowid = crate::varint::zigzag_decode(rowid_enc);
                let (old_row, n) = read_row(&data[off..])?;
                off += n;
                let (new_row, n) = read_row(&data[off..])?;
                off += n;
                Some((LogRecord::Update { txid, table_name, rowid, old_row, new_row }, off))
            }
            4 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let (table_name, n) = read_string(&data[off..])?;
                off += n;
                let (rowid_enc, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let rowid = crate::varint::zigzag_decode(rowid_enc);
                let has_row = *data.get(off)?;
                off += 1;
                let row = if has_row != 0 {
                    let (r, n) = read_row(&data[off..])?;
                    off += n;
                    Some(r)
                } else {
                    None
                };
                Some((LogRecord::Delete { txid, table_name, rowid, row }, off))
            }
            5 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                Some((LogRecord::Prepare(txid), off))
            }
            6 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                Some((LogRecord::Commit(txid), off))
            }
            7 => {
                let (txid, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                Some((LogRecord::Rollback(txid), off))
            }
            _ => None,
        }
    }
}

// ── Helper serialization / deserialization fns ─────────────────────────────

fn write_string(s: &str, buf: &mut Vec<u8>) -> Result<()> {
    let bytes = s.as_bytes();
    crate::varint::write_varint_u64(bytes.len() as u64, buf);
    buf.write_all(bytes).map_err(KkdbError::Io)
}

fn write_row(row: &Row, buf: &mut Vec<u8>) -> Result<()> {
    let mut row_buf = Vec::new();
    crate::types::serialize_row_into(row, &mut row_buf);
    crate::varint::write_varint_u64(row_buf.len() as u64, buf);
    buf.write_all(&row_buf).map_err(KkdbError::Io)
}

fn read_string(data: &[u8]) -> Option<(String, usize)> {
    let (len, n) = crate::varint::read_varint_u64(data).ok()?;
    let end = n + len as usize;
    if data.len() < end {
        return None;
    }
    let s = std::str::from_utf8(&data[n..end]).ok()?.to_string();
    Some((s, end))
}

fn read_row(data: &[u8]) -> Option<(Row, usize)> {
    let (len, n) = crate::varint::read_varint_u64(data).ok()?;
    let end = n + len as usize;
    if data.len() < end {
        return None;
    }
    let row = crate::types::deserialize_row(&data[n..end]).ok()?;
    Some((row, end))
}

// ── BinlogManager ──────────────────────────────────────────────────────────

/// Manages the append-only binlog file.
///
/// On-disk record format (per entry):
/// ```text
/// [record_len: u32 LE][crc32: u32 LE][record_data: record_len bytes]
/// ```
/// Checksums protect against torn writes during crash.
pub struct BinlogManager {
    /// Append-mode writer. None for in-memory databases.
    file: Option<BufWriter<File>>,
    /// Path to the binlog file. Needed for recovery (re-open as readable).
    path: Option<std::path::PathBuf>,
}

impl BinlogManager {
    /// Open or create a binlog file for a given database path.
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref();
        let binlog_path = db_path.with_extension("binlog");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&binlog_path)
            .map_err(KkdbError::Io)?;
        Ok(Self {
            file: Some(BufWriter::with_capacity(64 * 1024, file)),
            path: Some(binlog_path),
        })
    }

    /// Create a dummy binlog manager for in-memory databases.
    pub fn open_memory() -> Self {
        Self { file: None, path: None }
    }

    /// Append a record to the binlog buffer/file.
    pub fn append(&mut self, record: &LogRecord) -> Result<()> {
        if let Some(file) = &mut self.file {
            let mut record_buf = Vec::new();
            record.serialize(&mut record_buf)?;

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

    /// Flush the write buffer and call `fsync`.
    pub fn fsync(&mut self) -> Result<()> {
        if let Some(file) = &mut self.file {
            file.flush().map_err(KkdbError::Io)?;
            file.get_mut().sync_all().map_err(KkdbError::Io)?;
        }
        Ok(())
    }

    /// Crash-recovery: read the binlog sequentially, verify checksums, and
    /// determine which transactions were left uncommitted.
    ///
    /// ## Return value
    /// A `HashSet<u64>` of transaction IDs that reached PREPARE but not COMMIT/ROLLBACK.
    /// These transactions **must not** be re-applied (the COW pager already rolled back
    /// their incomplete writes). The binlog is truncated at the last valid record boundary.
    ///
    /// ## Recovery strategy
    /// For KKDB's COW pager, crash recovery is primarily handled at the storage layer:
    /// - If the superblock write was incomplete the pager loads the previous superblock.
    /// - The binlog's role is supplementary: it lets higher-level tools detect which
    ///   logical transactions were in-flight and optionally surface that information.
    pub fn recover(&mut self) -> Result<HashSet<u64>> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => return Ok(HashSet::new()), // In-memory: nothing to recover
        };

        // Flush buffered writer before reading
        if let Some(f) = &mut self.file {
            let _ = f.flush();
        }

        // Read entire file
        let content = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
            Err(e) => return Err(KkdbError::Io(e)),
        };

        let mut prepared: HashSet<u64> = HashSet::new(); // txids that hit PREPARE
        let mut committed: HashSet<u64> = HashSet::new(); // txids that hit COMMIT/ROLLBACK
        let mut pos = 0usize;
        let mut last_valid = 0usize; // last position after a successfully read record

        while pos + 8 <= content.len() {
            let record_len =
                u32::from_le_bytes(content[pos..pos + 4].try_into().unwrap()) as usize;
            let stored_crc =
                u32::from_le_bytes(content[pos + 4..pos + 8].try_into().unwrap());
            let data_start = pos + 8;
            let data_end = data_start + record_len;

            if data_end > content.len() {
                // Torn write at tail — stop here
                break;
            }

            let actual_crc = crc32fast::hash(&content[data_start..data_end]);
            if actual_crc != stored_crc {
                // Checksum mismatch — stop here
                break;
            }

            // Parse the record payload
            match LogRecord::deserialize(&content[data_start..data_end], 0) {
                Some((record, _)) => {
                    match &record {
                        LogRecord::Prepare(txid) => {
                            prepared.insert(*txid);
                        }
                        LogRecord::Commit(txid) | LogRecord::Rollback(txid) => {
                            committed.insert(*txid);
                        }
                        _ => {}
                    }
                    pos = data_end;
                    last_valid = pos;
                }
                None => break, // Unparseable — treat as corruption
            }
        }

        // Truncate any partially-written tail records
        if last_valid < content.len() {
            if let Ok(f) = OpenOptions::new().write(true).open(&path) {
                let _ = f.set_len(last_valid as u64);
                let _ = f.sync_all();
            }
        }

        // Return txids that were prepared but never committed
        let uncommitted: HashSet<u64> = prepared.difference(&committed).copied().collect();
        Ok(uncommitted)
    }
}
