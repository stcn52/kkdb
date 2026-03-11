use crate::error::{KkdbError, Result};
use crate::types::Row;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

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
    /// A complete SQL statement committed via Raft consensus.
    ///
    /// Used for **statement-based replication**: every `KkdbRequest` that is
    /// applied by the Raft StateMachine is also appended here so that
    /// `BinlogFollower`s can replay the exact SQL against their local VM.
    ///
    /// `user_id` is empty for admin/system SQL.
    Sql {
        /// The SQL statement string.
        sql: String,
        /// Owner user-id (empty = system/auth VM).
        user_id: String,
        /// Raft log index of the entry that committed this statement.
        raft_index: u64,
    },
}

impl LogRecord {
    // ── Serialization ──────────────────────────────────────────────────────

    pub fn serialize(&self, buf: &mut Vec<u8>) -> Result<()> {
        match self {
            LogRecord::Begin(txid) => {
                buf.push(1);
                crate::varint::write_varint_u64(*txid, buf);
            }
            LogRecord::Insert {
                txid,
                table_name,
                rowid,
                row,
            } => {
                buf.push(2);
                crate::varint::write_varint_u64(*txid, buf);
                write_string(table_name, buf)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                write_row(row, buf)?;
            }
            LogRecord::Update {
                txid,
                table_name,
                rowid,
                old_row,
                new_row,
            } => {
                buf.push(3);
                crate::varint::write_varint_u64(*txid, buf);
                write_string(table_name, buf)?;
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*rowid), buf);
                write_row(old_row, buf)?;
                write_row(new_row, buf)?;
            }
            LogRecord::Delete {
                txid,
                table_name,
                rowid,
                row,
            } => {
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
            LogRecord::Sql {
                sql,
                user_id,
                raft_index,
            } => {
                buf.push(8);
                crate::varint::write_varint_u64(*raft_index, buf);
                write_string(sql, buf)?;
                write_string(user_id, buf)?;
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
                Some((
                    LogRecord::Insert {
                        txid,
                        table_name,
                        rowid,
                        row,
                    },
                    off,
                ))
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
                Some((
                    LogRecord::Update {
                        txid,
                        table_name,
                        rowid,
                        old_row,
                        new_row,
                    },
                    off,
                ))
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
                Some((
                    LogRecord::Delete {
                        txid,
                        table_name,
                        rowid,
                        row,
                    },
                    off,
                ))
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
            8 => {
                let (raft_index, n) = crate::varint::read_varint_u64(&data[off..]).ok()?;
                off += n;
                let (sql, n) = read_string(&data[off..])?;
                off += n;
                let (user_id, n) = read_string(&data[off..])?;
                off += n;
                Some((
                    LogRecord::Sql {
                        sql,
                        user_id,
                        raft_index,
                    },
                    off,
                ))
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
    /// Monotonically increasing write position (byte offset after last committed record).
    pub write_pos: u64,
    /// In-memory frame buffer — used when `file` is None (tests / in-memory mode).
    mem_buf: Vec<u8>,
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
        let initial_pos = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            file: Some(BufWriter::with_capacity(64 * 1024, file)),
            path: Some(binlog_path),
            write_pos: initial_pos,
            mem_buf: Vec::new(),
        })
    }

    /// Create a dummy binlog manager for in-memory databases.
    pub fn open_memory() -> Self {
        Self {
            file: None,
            path: None,
            write_pos: 0,
            mem_buf: Vec::new(),
        }
    }

    /// Append a record to the binlog buffer/file.
    /// Returns the byte offset of the START of this framed record (for streaming).
    pub fn append(&mut self, record: &LogRecord) -> Result<u64> {
        let start_pos = self.write_pos;

        let mut record_buf = Vec::new();
        record.serialize(&mut record_buf)?;
        let total_len = record_buf.len() as u32;
        let checksum = crc32fast::hash(&record_buf);
        let mut header = [0u8; 8];
        header[0..4].copy_from_slice(&total_len.to_le_bytes());
        header[4..8].copy_from_slice(&checksum.to_le_bytes());

        if let Some(file) = &mut self.file {
            file.write_all(&header).map_err(KkdbError::Io)?;
            file.write_all(&record_buf).map_err(KkdbError::Io)?;
        } else {
            // In-memory mode: buffer the framed bytes for read_from()
            self.mem_buf.extend_from_slice(&header);
            self.mem_buf.extend_from_slice(&record_buf);
        }
        self.write_pos += 8 + record_buf.len() as u64;
        Ok(start_pos)
    }

    /// Flush the write buffer and call `fsync`.
    pub fn fsync(&mut self) -> Result<()> {
        if let Some(file) = &mut self.file {
            file.flush().map_err(KkdbError::Io)?;
            file.get_mut().sync_all().map_err(KkdbError::Io)?;
        }
        Ok(())
    }

    /// Read all framed records starting at byte `from_pos`.
    ///
    /// Returns `Vec<(pos_after_record, framed_bytes)>` covering both file-backed
    /// and in-memory mode. In in-memory mode, reads from `mem_buf`.
    pub fn read_from(&self, from_pos: u64) -> Result<Vec<(u64, Vec<u8>)>> {
        // In-memory mode: scan mem_buf
        if self.path.is_none() {
            return Ok(Self::scan_frames(&self.mem_buf, from_pos));
        }

        // SAFETY: early return above guarantees self.path is Some
        let path = self.path.as_ref().unwrap();

        let content = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(KkdbError::Io(e)),
        };

        Ok(Self::scan_frames(&content, from_pos))
    }

    /// Scan raw frame bytes starting at byte offset `from_pos`.
    ///
    /// Shared by both file-backed and in-memory `read_from` paths.
    fn scan_frames(content: &[u8], from_pos: u64) -> Vec<(u64, Vec<u8>)> {
        let mut pos = from_pos as usize;
        let mut results = Vec::new();

        while pos + 8 <= content.len() {
            // SAFETY: loop guard ensures pos..pos+4 and pos+4..pos+8 are valid 4-byte slices
            let record_len = u32::from_le_bytes(content[pos..pos + 4].try_into().unwrap()) as usize;
            let stored_crc = u32::from_le_bytes(content[pos + 4..pos + 8].try_into().unwrap());
            let data_start = pos + 8;
            let data_end = data_start + record_len;

            if data_end > content.len() {
                break; // Torn write at tail
            }

            let actual_crc = crc32fast::hash(&content[data_start..data_end]);
            if actual_crc != stored_crc {
                break; // Checksum mismatch
            }

            let framed = content[pos..data_end].to_vec();
            let next_pos = data_end as u64;
            results.push((next_pos, framed));
            pos = data_end;
        }

        results
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
            // SAFETY: loop guard ensures pos..pos+4 and pos+4..pos+8 are valid 4-byte slices
            let record_len = u32::from_le_bytes(content[pos..pos + 4].try_into().unwrap()) as usize;
            let stored_crc = u32::from_le_bytes(content[pos + 4..pos + 8].try_into().unwrap());
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

        // Update write_pos to reflect the actual valid content length
        self.write_pos = last_valid as u64;

        // Return txids that were prepared but never committed
        let uncommitted: HashSet<u64> = prepared.difference(&committed).copied().collect();
        Ok(uncommitted)
    }
}

// ── BinlogBroadcaster ─────────────────────────────────────────────────────────
//
// Wraps BinlogManager with a tokio broadcast channel so that multiple subscribers
// (e.g. replication followers, CDC consumers) can receive binlog events in real-time.
//
// Usage (Leader side):
//   let broadcaster = BinlogBroadcaster::new(manager);
//   let sub = broadcaster.subscribe();          // hand to follower task
//   broadcaster.append_and_broadcast(&record);  // called from write path

/// A framed binlog event ready for streaming.
///
/// `pos` is the byte offset *after* this record in the binlog file — the
/// receiver uses it as the next `from_pos` for incremental pull.
#[derive(Debug, Clone)]
pub struct BinlogEvent {
    /// Byte offset after this event (next pull position).
    pub pos: u64,
    /// Raw framed bytes: `[record_len: u32 LE][crc32: u32 LE][payload]`.
    pub framed: Vec<u8>,
}

/// Thread-safe broadcaster that wraps `BinlogManager` and fans out every new
/// record to all active subscribers via a tokio broadcast channel.
#[derive(Clone)]
pub struct BinlogBroadcaster {
    pub manager: Arc<Mutex<BinlogManager>>,
    tx: tokio::sync::broadcast::Sender<BinlogEvent>,
}

impl BinlogBroadcaster {
    /// Create a new broadcaster with the given manager.
    ///
    /// `capacity` controls the number of buffered events (backpressure for slow subscribers).
    pub fn new(manager: BinlogManager, capacity: usize) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(capacity);
        Self {
            manager: Arc::new(Mutex::new(manager)),
            tx,
        }
    }

    /// Convenience constructor for in-memory / test mode.
    pub fn in_memory() -> Self {
        Self::new(BinlogManager::open_memory(), 1024)
    }

    /// Subscribe to live binlog events.
    ///
    /// The subscriber will receive every event appended *after* the call to this
    /// method. For catch-up of historical records, use `BinlogManager::read_from`.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BinlogEvent> {
        self.tx.subscribe()
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Append a record, persist it to the binlog, and broadcast to all subscribers.
    ///
    /// Returns the byte offset of the START of this record (useful for bookkeeping).
    pub fn append_and_broadcast(&self, record: &LogRecord) -> crate::error::Result<u64> {
        let (start_pos, framed) = {
            let mut mgr = self.manager.lock().unwrap_or_else(|e| e.into_inner());
            let start_pos = mgr.append(record)?;

            // Re-read the framed bytes we just wrote so we can broadcast them
            let frame_len = mgr.write_pos - start_pos;
            let path = mgr.path.clone();
            drop(mgr); // release lock before potentially slow I/O

            // Read back the exact bytes we just wrote; use the path or reconstruct
            // the frame in memory for in-memory mode.
            let framed = if let Some(p) = path {
                // Flush first so the bytes are visible
                {
                    let mut mgr = self.manager.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = mgr.fsync();
                }
                let content = std::fs::read(&p)?;
                let end = (start_pos + frame_len) as usize;
                let begin = start_pos as usize;
                if end <= content.len() {
                    content[begin..end].to_vec()
                } else {
                    vec![]
                }
            } else {
                // In-memory: reconstruct the packet directly
                let mut buf = Vec::new();
                let mut inner = Vec::new();
                record.serialize(&mut inner).ok();
                let len = inner.len() as u32;
                let crc = crc32fast::hash(&inner);
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(&crc.to_le_bytes());
                buf.extend_from_slice(&inner);
                buf
            };

            let pos = start_pos + framed.len() as u64;
            (start_pos, (pos, framed))
        };

        let event = BinlogEvent {
            pos: framed.0,
            framed: framed.1,
        };
        // Ignore send errors — no active subscribers is fine
        let _ = self.tx.send(event);
        Ok(start_pos)
    }
}

// ── BinlogFollower ────────────────────────────────────────────────────────────
//
// Runs on a Follower node. Pulls binlog records from the Leader's HTTP streaming
// endpoint, decodes them, and replays the SQL through the local VM.

/// Incremental pull client for followers.
///
/// Calls `GET {leader_url}/binlog/stream?from_pos={pos}` in a loop.
/// Each response line is a JSON object: `{"pos": u64, "data": "<base64>"}`.
///
/// On receiving a record the follower:
///   1. Base64-decodes the `data` field to get the framed bytes.
///   2. Strips the 8-byte header (len + crc32).
///   3. Calls `LogRecord::deserialize` on the payload.
///   4. Converts the record to SQL and executes it on the local VM.
///   5. Persists the new `pos` as the checkpoint so it can resume after restart.
pub struct BinlogFollower {
    /// Base HTTP URL of the Leader (e.g. `"http://127.0.0.1:6543"`).
    pub leader_url: String,
    /// Current pull position (byte offset into the Leader's binlog).
    pub pos: u64,
    /// Checkpoint path — stores the current pos so restarts can resume.
    pub checkpoint_path: Option<std::path::PathBuf>,
}

impl BinlogFollower {
    /// Create a new follower client.
    ///
    /// `leader_url`       — leader's REST base URL.  
    /// `checkpoint_path`  — path to a file that persists `pos` across restarts.
    pub fn new(leader_url: String, checkpoint_path: Option<std::path::PathBuf>) -> Self {
        let pos = checkpoint_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        Self {
            leader_url,
            pos,
            checkpoint_path,
        }
    }

    /// Persist the current position to disk.
    fn save_checkpoint(&self) {
        if let Some(p) = &self.checkpoint_path {
            let _ = std::fs::write(p, self.pos.to_string());
        }
    }

    /// Pull one batch of records from the leader (`GET /binlog/stream?from_pos=…`).
    ///
    /// Returns the list of decoded records + the new position.
    /// Returns an empty vec if no new records are available.
    pub async fn pull_batch(&self) -> std::result::Result<Vec<(u64, LogRecord)>, String> {
        let url = format!("{}/binlog/stream?from_pos={}", self.leader_url, self.pos);
        let resp = reqwest::get(&url)
            .await
            .map_err(|e| format!("HTTP pull failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("leader returned {}", resp.status()));
        }

        let body = resp.text().await.map_err(|e| format!("read body: {e}"))?;
        let mut results = Vec::new();

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Each line: {"pos":12345,"data":"base64..."}
            let obj: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("json parse: {e}"))?;
            let pos = obj["pos"].as_u64().unwrap_or(0);
            let data_b64 = obj["data"].as_str().unwrap_or("");
            let framed =
                base64_decode(data_b64).ok_or_else(|| "base64 decode error".to_string())?;

            // Strip 8-byte header (record_len u32 + crc32 u32)
            if framed.len() < 8 {
                continue;
            }
            let payload = &framed[8..];
            if let Some((record, _)) = LogRecord::deserialize(payload, 0) {
                results.push((pos, record));
            }
        }

        Ok(results)
    }

    /// Run the follower replication loop.
    ///
    /// Calls `pull_batch` in a loop, replaying records against `apply_fn`.
    /// `apply_fn(record)` should execute the record's SQL against the local VM.
    ///
    /// The loop sleeps `poll_interval` between empty batches and retries
    /// `retry_interval` after errors. Never returns unless the future is cancelled.
    pub async fn run_loop<F>(
        &mut self,
        mut apply_fn: F,
        poll_interval: std::time::Duration,
        retry_interval: std::time::Duration,
    ) where
        F: FnMut(&LogRecord),
    {
        loop {
            match self.pull_batch().await {
                Ok(records) if records.is_empty() => {
                    // No new data — wait and poll again
                    tokio::time::sleep(poll_interval).await;
                }
                Ok(records) => {
                    for (pos, record) in records {
                        apply_fn(&record);
                        self.pos = pos;
                    }
                    self.save_checkpoint();
                }
                Err(e) => {
                    eprintln!("[BinlogFollower] pull error: {e}; retry in {retry_interval:?}");
                    tokio::time::sleep(retry_interval).await;
                }
            }
        }
    }

    /// Convert a `LogRecord` to the equivalent SQL statement(s) for replay.
    ///
    /// Called by `apply_fn`; exposed here so callers can inspect the SQL.
    pub fn record_to_sql(record: &LogRecord) -> Vec<String> {
        match record {
            LogRecord::Begin(txid) => {
                vec![format!("-- BEGIN txid={txid}")]
            }
            LogRecord::Insert {
                table_name,
                rowid: _rowid,
                row,
                ..
            } => {
                let cols: Vec<String> = (0..row.len()).map(|i| format!("col{i}")).collect();
                let vals: Vec<String> = row.iter().map(value_to_sql_literal).collect();
                vec![format!(
                    "INSERT OR REPLACE INTO {table_name} ({cols}) VALUES ({vals})",
                    cols = cols.join(", "),
                    vals = vals.join(", "),
                )]
            }
            LogRecord::Update {
                table_name,
                rowid,
                new_row,
                ..
            } => {
                let sets: Vec<String> = new_row
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("col{i} = {}", value_to_sql_literal(v)))
                    .collect();
                vec![format!(
                    "UPDATE {table_name} SET {sets} WHERE rowid = {rowid}",
                    sets = sets.join(", "),
                )]
            }
            LogRecord::Delete {
                table_name, rowid, ..
            } => {
                vec![format!("DELETE FROM {table_name} WHERE rowid = {rowid}")]
            }
            LogRecord::Commit(txid) => vec![format!("-- COMMIT txid={txid}")],
            LogRecord::Rollback(txid) => vec![format!("-- ROLLBACK txid={txid}")],
            LogRecord::Prepare(txid) => vec![format!("-- PREPARE txid={txid}")],
            LogRecord::Sql { sql, .. } => vec![sql.clone()],
        }
    }
}

fn value_to_sql_literal(v: &crate::types::Value) -> String {
    match v {
        crate::types::Value::Null => "NULL".into(),
        crate::types::Value::Integer(i) => i.to_string(),
        crate::types::Value::Real(f) => f.to_string(),
        crate::types::Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        crate::types::Value::Blob(b) => format!("X'{}'", hex_encode(b)),
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    // Minimal RFC 4648 base64 decoder (no external crate needed)
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity((s.len() * 3) / 4);
    let bytes = s.as_bytes();
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        let val = TABLE.iter().position(|&t| t == b)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        result.push(CHARS[b0 >> 2] as char);
        result.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        result.push(if chunk.len() > 1 {
            CHARS[((b1 & 0xf) << 2) | (b2 >> 6)] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            CHARS[b2 & 0x3f] as char
        } else {
            '='
        });
    }
    result
}
