//! Raft distributed consensus module for KKDB.
//!
//! This module provides a multi-node, strongly consistent cluster using
//! `openraft v0.9` with KKDB's VM as the state machine.
//!
//! # Modules
//!
//! - `types`      — type configuration, request/response structs
//! - `log_store`  — unified RaftStorage (log + state machine + snapshots)
//! - `network`    — in-memory channel-based RPC transport (Phase 1)
//! - `node`       — high-level KkdbNode API and cluster utilities

// ── Core Raft consensus (stay at root) ─────────────────────────────────
pub mod http_network;
pub mod http_transport;
pub mod log_store;
pub mod network;
pub mod node;
pub mod state_machine;
pub mod types;

// ── Feature modules ───────────────────────────────────────────────────
pub mod features;     // ha, ha_dr, dtx, dist_txn, consistent_hash, snapshot_isolation, cluster_mgmt

// ── Backward-compatible re-exports ────────────────────────────────────
pub use features::consistent_hash;
pub use features::dtx;
pub use features::ha;
pub use features::snapshot_isolation;
pub use features::cluster_mgmt;
pub use features::dist_txn;
pub use features::ha_dr;

pub use node::{start_cluster_3, KkdbNode, KkdbRaft};
pub use types::{KkdbNodeId, KkdbRequest, KkdbResponse, KkdbTypeConfig};
