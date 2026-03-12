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

pub mod consistent_hash;
pub mod dtx;
pub mod ha;
pub mod snapshot_isolation;
pub mod cluster_mgmt;
pub mod dist_txn;
pub mod http_network;
pub mod http_transport;
pub mod log_store;
pub mod network;
pub mod node;
pub mod state_machine;
pub mod types;

pub use node::{start_cluster_3, KkdbNode, KkdbRaft};
pub use types::{KkdbNodeId, KkdbRequest, KkdbResponse, KkdbTypeConfig};
