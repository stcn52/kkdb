//! State machine for KKDB Raft (openraft v0.9 + storage-v2).
//!
//! `KkdbStateMachine` implements `RaftStateMachine`. When the Raft consensus
//! engine commits a log entry it calls `apply()` here, which routes the SQL
//! to the correct per-user KKDB VM and executes it.
//!
//! ## Snapshot persistence
//!
//! When `snapshot_dir` is `Some(dir)`:
//!   - `build_snapshot` → JSON-serialises `KkdbSnapshotData` + meta, writes
//!     atomically to `{dir}/raft/snapshot.json` via a `.tmp` rename.
//!   - `get_current_snapshot` → reads and deserialises `snapshot.json`.
//!   - `install_snapshot` → saves the incoming blob to disk, then replays SQL.
//!
//! When `snapshot_dir` is `None` (tests / in-memory mode) snapshots are
//! purely in-memory with no disk I/O.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use openraft::{
    storage::RaftStateMachine, Entry, EntryPayload, LogId, RaftSnapshotBuilder, Snapshot,
    SnapshotMeta, StorageError, StorageIOError, StoredMembership,
};
use serde::{Deserialize, Serialize};

use crate::binlog::{BinlogBroadcaster, LogRecord};
use crate::raft::types::{KkdbNodeId, KkdbRequest, KkdbResponse, KkdbTypeConfig};
use crate::server::http_api::AppState;
use crate::vm::execute::VM;

// ─── On-disk snapshot format ──────────────────────────────────────────────────

/// What is persisted to `snapshot.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PersistedSnapshot {
    pub meta: SerializedSnapshotMeta,
    pub data: KkdbSnapshotData,
}

/// A serialisable copy of `SnapshotMeta` (which contains non-Serialize fields
/// in some openraft versions, so we extract what we need manually).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SerializedSnapshotMeta {
    pub last_log_id: Option<LogId<KkdbNodeId>>,
    pub last_membership: StoredMembership<KkdbNodeId, openraft::BasicNode>,
    pub snapshot_id: String,
}

/// The actual payload carried inside a snapshot.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct KkdbSnapshotData {
    /// All SQL writes that have been applied, in order.
    pub entries: Vec<KkdbRequest>,
    pub last_applied: Option<LogId<KkdbNodeId>>,
    pub last_membership: StoredMembership<KkdbNodeId, openraft::BasicNode>,
}

// ─── State machine ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct KkdbStateMachine {
    pub app_state: AppState,
    pub last_applied_log: Option<LogId<KkdbNodeId>>,
    pub last_membership: StoredMembership<KkdbNodeId, openraft::BasicNode>,
    /// Running log of applied SQL requests (used for snapshot building).
    pub applied_entries: Vec<KkdbRequest>,
    pub snapshot_idx: u64,
    /// Root directory for snapshot files. `None` = in-memory only.
    pub snapshot_dir: Option<PathBuf>,
    /// Cached last snapshot meta+data for `get_current_snapshot`.
    pub current_snapshot: Option<PersistedSnapshot>,
    /// Binlog broadcaster. When set, every committed Raft write is also
    /// appended to the binlog and fanned out to all subscribers.
    pub binlog: Option<BinlogBroadcaster>,
}

impl KkdbStateMachine {
    /// Create an in-memory state machine (tests).
    pub fn new(app_state: AppState) -> Self {
        Self {
            app_state,
            last_applied_log: None,
            last_membership: StoredMembership::default(),
            applied_entries: Vec::new(),
            snapshot_idx: 0,
            snapshot_dir: None,
            current_snapshot: None,
            binlog: None,
        }
    }

    /// Create a persistent state machine backed by `dir`.
    ///
    /// On construction it tries to load an existing snapshot from disk and
    /// replay its SQL to restore the VM state.
    pub fn open(app_state: AppState, dir: &Path) -> std::io::Result<Self> {
        let snapshot_dir = dir.join("raft");
        std::fs::create_dir_all(&snapshot_dir)?;

        let mut sm = Self {
            app_state,
            last_applied_log: None,
            last_membership: StoredMembership::default(),
            applied_entries: Vec::new(),
            snapshot_idx: 0,
            snapshot_dir: Some(snapshot_dir.clone()),
            current_snapshot: None,
            binlog: None, // caller sets this after open()
        };

        // Load snapshot from disk if it exists
        let snap_path = snapshot_dir.join("snapshot.json");
        if snap_path.exists() {
            let bytes = std::fs::read(&snap_path)?;
            if let Ok(persisted) = serde_json::from_slice::<PersistedSnapshot>(&bytes) {
                sm.last_applied_log = persisted.data.last_applied;
                sm.last_membership = persisted.data.last_membership.clone();
                sm.applied_entries = persisted.data.entries.clone();
                sm.current_snapshot = Some(persisted.clone());

                // Replay snapshot SQL to restore VM state
                for req in &persisted.data.entries {
                    sm.apply_request(req);
                }
            }
        }
        Ok(sm)
    }

    /// Route a SQL request to the correct VM and execute it.
    pub fn apply_request(&self, req: &KkdbRequest) -> KkdbResponse {
        let vm_arc = if req.user_id.is_empty() {
            Arc::clone(&self.app_state.auth_vm)
        } else {
            let mut cache = self.app_state.user_vms.lock().unwrap();
            if let Some(vm) = cache.get(&req.user_id) {
                Arc::clone(vm)
            } else {
                let vm = match &self.app_state.data_dir {
                    Some(base) => {
                        let path = base.as_ref().join(&req.user_id);
                        match VM::open(&path.to_string_lossy()) {
                            Ok(v) => v,
                            Err(e) => {
                                return KkdbResponse {
                                    message: e.to_string(),
                                    ok: false,
                                }
                            }
                        }
                    }
                    None => VM::new_memory(),
                };
                let arc = Arc::new(Mutex::new(vm));
                cache.insert(req.user_id.clone(), Arc::clone(&arc));
                arc
            }
        };
        let mut vm = vm_arc.lock().unwrap();
        match vm.execute_sql(&req.sql) {
            Ok(r) => KkdbResponse {
                message: format!("{r:?}"),
                ok: true,
            },
            Err(e) => KkdbResponse {
                message: e.to_string(),
                ok: false,
            },
        }
    }

    // ── Disk I/O helpers ──────────────────────────────────────────────────────

    fn write_snapshot_to_disk(dir: &Path, persisted: &PersistedSnapshot) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(persisted).map_err(std::io::Error::other)?;
        // Atomic write: write to .tmp then rename
        let tmp_path = dir.join("snapshot.tmp");
        let snap_path = dir.join("snapshot.json");
        std::fs::write(&tmp_path, &bytes)?;
        std::fs::rename(&tmp_path, &snap_path)?;
        Ok(())
    }

    fn read_snapshot_from_disk(dir: &Path) -> std::io::Result<Option<PersistedSnapshot>> {
        let snap_path = dir.join("snapshot.json");
        if !snap_path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&snap_path)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

// ─── RaftSnapshotBuilder ──────────────────────────────────────────────────────

impl RaftSnapshotBuilder<KkdbTypeConfig> for KkdbStateMachine {
    async fn build_snapshot(
        &mut self,
    ) -> Result<Snapshot<KkdbTypeConfig>, StorageError<KkdbNodeId>> {
        self.snapshot_idx += 1;
        let snap_data = KkdbSnapshotData {
            entries: self.applied_entries.clone(),
            last_applied: self.last_applied_log,
            last_membership: self.last_membership.clone(),
        };
        let snap_id = match self.last_applied_log {
            Some(l) => format!("{}-{}-{}", l.leader_id, l.index, self.snapshot_idx),
            None => format!("empty-{}", self.snapshot_idx),
        };
        let meta = SnapshotMeta {
            last_log_id: self.last_applied_log,
            last_membership: self.last_membership.clone(),
            snapshot_id: snap_id.clone(),
        };

        // Build the persisted form (meta + data as one JSON blob)
        let persisted = PersistedSnapshot {
            meta: SerializedSnapshotMeta {
                last_log_id: meta.last_log_id,
                last_membership: meta.last_membership.clone(),
                snapshot_id: snap_id,
            },
            data: snap_data,
        };

        // Write to disk (if configured)
        if let Some(ref dir) = self.snapshot_dir.clone() {
            Self::write_snapshot_to_disk(dir, &persisted)
                .map_err(|e| StorageIOError::write_snapshot(Some(meta.signature()), &e))?;
        }
        self.current_snapshot = Some(persisted.clone());

        let bytes = serde_json::to_vec(&persisted).unwrap_or_default();
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        })
    }
}

// ─── RaftStateMachine ─────────────────────────────────────────────────────────

impl RaftStateMachine<KkdbTypeConfig> for KkdbStateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<
        (
            Option<LogId<KkdbNodeId>>,
            StoredMembership<KkdbNodeId, openraft::BasicNode>,
        ),
        StorageError<KkdbNodeId>,
    > {
        Ok((self.last_applied_log, self.last_membership.clone()))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<KkdbResponse>, StorageError<KkdbNodeId>>
    where
        I: IntoIterator<Item = Entry<KkdbTypeConfig>> + openraft::OptionalSend,
        I::IntoIter: openraft::OptionalSend,
    {
        let mut replies = Vec::new();
        for entry in entries {
            self.last_applied_log = Some(entry.log_id);
            let raft_index = entry.log_id.index;

            let resp = match &entry.payload {
                EntryPayload::Blank => KkdbResponse {
                    message: "blank".into(),
                    ok: true,
                },
                EntryPayload::Normal(req) => {
                    self.applied_entries.push(req.clone());
                    let resp = self.apply_request(req);

                    // ── Binlog: emit Sql record for every committed write ────────────
                    if let Some(ref broadcaster) = self.binlog {
                        let record = LogRecord::Sql {
                            sql: req.sql.clone(),
                            user_id: req.user_id.clone(),
                            raft_index,
                        };
                        // Best-effort: never fail Raft apply due to binlog errors
                        if let Err(e) = broadcaster.append_and_broadcast(&record) {
                            eprintln!("[Binlog] append error at raft_index={raft_index}: {e}");
                        }
                    }

                    resp
                }
                EntryPayload::Membership(mem) => {
                    self.last_membership = StoredMembership::new(Some(entry.log_id), mem.clone());
                    KkdbResponse {
                        message: "membership".into(),
                        ok: true,
                    }
                }
            };
            replies.push(resp);
        }
        Ok(replies)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<KkdbNodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<KkdbNodeId, openraft::BasicNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<KkdbNodeId>> {
        let bytes = snapshot.into_inner();

        // Try to parse as full PersistedSnapshot (leader sends it); or as
        // bare KkdbSnapshotData for backward compat.
        let persisted: PersistedSnapshot = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            let data: KkdbSnapshotData = serde_json::from_slice(&bytes).unwrap_or_default();
            PersistedSnapshot {
                meta: SerializedSnapshotMeta {
                    last_log_id: meta.last_log_id,
                    last_membership: meta.last_membership.clone(),
                    snapshot_id: meta.snapshot_id.clone(),
                },
                data,
            }
        });

        // Persist to disk (atomic)
        if let Some(ref dir) = self.snapshot_dir.clone() {
            Self::write_snapshot_to_disk(dir, &persisted)
                .map_err(|e| StorageIOError::write_snapshot(Some(meta.signature()), &e))?;
        }

        // Update in-memory state
        self.last_applied_log = persisted.data.last_applied;
        self.last_membership = persisted.data.last_membership.clone();
        self.applied_entries = persisted.data.entries.clone();
        self.current_snapshot = Some(persisted.clone());

        // Replay SQL to restore VM state
        for req in &persisted.data.entries {
            self.apply_request(req);
        }
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<KkdbTypeConfig>>, StorageError<KkdbNodeId>> {
        // Try in-memory cache first
        if let Some(ref persisted) = self.current_snapshot {
            let bytes = serde_json::to_vec(persisted).unwrap_or_default();
            let meta = SnapshotMeta {
                last_log_id: persisted.meta.last_log_id,
                last_membership: persisted.meta.last_membership.clone(),
                snapshot_id: persisted.meta.snapshot_id.clone(),
            };
            return Ok(Some(Snapshot {
                meta,
                snapshot: Box::new(Cursor::new(bytes)),
            }));
        }

        // Fall back to disk
        if let Some(ref dir) = self.snapshot_dir.clone() {
            match Self::read_snapshot_from_disk(dir) {
                Ok(Some(persisted)) => {
                    let bytes = serde_json::to_vec(&persisted).unwrap_or_default();
                    let meta = SnapshotMeta {
                        last_log_id: persisted.meta.last_log_id,
                        last_membership: persisted.meta.last_membership.clone(),
                        snapshot_id: persisted.meta.snapshot_id.clone(),
                    };
                    self.current_snapshot = Some(persisted);
                    return Ok(Some(Snapshot {
                        meta,
                        snapshot: Box::new(Cursor::new(bytes)),
                    }));
                }
                Ok(None) => {}
                Err(e) => {
                    // Log but don't fail — a missing snapshot is recoverable via log replay
                    eprintln!("[Raft] Warning: could not read snapshot from disk: {e}");
                }
            }
        }

        Ok(None)
    }
}
