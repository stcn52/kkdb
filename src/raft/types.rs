//! Raft type configuration and application request types for KKDB.

use openraft::BasicNode;
use serde::{Deserialize, Serialize};

/// The application-level "request" that gets replicated through Raft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KkdbRequest {
    /// SQL statement (DML or DDL) to replicate
    pub sql: String,
    /// The user_id whose isolated VM should execute the SQL.
    /// Empty string = execute against the global auth VM.
    pub user_id: String,
}

/// The application-level "response" returned after apply().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KkdbResponse {
    pub message: String,
    pub ok: bool,
}

/// Node ID type for the Raft cluster.
pub type KkdbNodeId = u64;

openraft::declare_raft_types!(
    /// KKDB's Raft type configuration.
    pub KkdbTypeConfig:
        D      = KkdbRequest,
        R      = KkdbResponse,
        Node   = BasicNode,
        NodeId = KkdbNodeId,
        Entry  = openraft::Entry<KkdbTypeConfig>,
        SnapshotData = std::io::Cursor<Vec<u8>>
);
